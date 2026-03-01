use crate::config::Config;
use crate::output;
use std::path::{Path, PathBuf};
use std::process::Command;

// ── Mount types ────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Mount {
    RoBind { src: PathBuf, dest: PathBuf },
    Bind { src: PathBuf, dest: PathBuf },
    DevBind { src: PathBuf, dest: PathBuf },
    Dev { dest: PathBuf },
    Proc { dest: PathBuf },
    Tmpfs { dest: PathBuf },
    Symlink { src: String, dest: PathBuf },
    FileRoBind { src: PathBuf, dest: PathBuf },
}

impl Mount {
    fn to_args(&self) -> Vec<String> {
        match self {
            Mount::RoBind { src, dest } | Mount::FileRoBind { src, dest } => {
                vec!["--ro-bind".into(), src.display().to_string(), dest.display().to_string()]
            }
            Mount::Bind { src, dest } => {
                vec!["--bind".into(), src.display().to_string(), dest.display().to_string()]
            }
            Mount::DevBind { src, dest } => {
                vec!["--dev-bind".into(), src.display().to_string(), dest.display().to_string()]
            }
            Mount::Dev { dest } => {
                vec!["--dev".into(), dest.display().to_string()]
            }
            Mount::Proc { dest } => {
                vec!["--proc".into(), dest.display().to_string()]
            }
            Mount::Tmpfs { dest } => {
                vec!["--tmpfs".into(), dest.display().to_string()]
            }
            Mount::Symlink { src, dest } => {
                vec!["--symlink".into(), src.clone(), dest.display().to_string()]
            }
        }
    }
}

struct MountSet {
    base: Vec<Mount>,
    home_dotfiles: Vec<Mount>,
    config_hide: Vec<Mount>,
    cache_hide: Vec<Mount>,
    local_overrides: Vec<Mount>,
    gpu: Vec<Mount>,
    docker: Vec<Mount>,
    shm: Vec<Mount>,
    display: Vec<Mount>,
    display_env: Vec<(String, String)>,
    extra: Vec<Mount>,
    project: Vec<Mount>,
}

impl MountSet {
    fn ordered_mounts(&self) -> [&[Mount]; 11] {
        [
            &self.base,
            &self.gpu,
            &self.shm,
            &self.docker,
            &self.display,
            &self.home_dotfiles,
            &self.config_hide,
            &self.cache_hide,
            &self.local_overrides,
            &self.extra,
            &self.project,
        ]
    }

    fn all_mount_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for group in self.ordered_mounts() {
            for m in group {
                args.extend(m.to_args());
            }
        }
        args
    }

    fn isolation_args(&self, project_dir: &Path) -> Vec<String> {
        let mut args = vec![
            "--chdir".into(),
            project_dir.display().to_string(),
            "--die-with-parent".into(),
            "--unshare-pid".into(),
            "--unshare-uts".into(),
            "--unshare-ipc".into(),
            "--hostname".into(),
            "ai-sandbox".into(),
        ];

        for (key, val) in &self.display_env {
            args.push("--setenv".into());
            args.push(key.clone());
            args.push(val.clone());
        }

        args.extend([
            "--setenv".into(), "PS1".into(), "(jail) \\w \\$ ".into(),
            "--setenv".into(), "_ZO_DOCTOR".into(), "0".into(),
        ]);

        args
    }
}

// ── SandboxGuard (RAII for temp hosts file) ────────────────────

pub struct SandboxGuard {
    hosts_path: PathBuf,
}

impl SandboxGuard {
    fn hosts_path(&self) -> &Path {
        &self.hosts_path
    }
}

impl Drop for SandboxGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.hosts_path);
    }
}

#[cfg(test)]
impl SandboxGuard {
    fn test_with_hosts(path: PathBuf) -> Self {
        SandboxGuard { hosts_path: path }
    }
}

// ── Constants ──────────────────────────────────────────────────

// Subdirs of ~/.config to hide (tmpfs over rw config mount)
const CONFIG_DENY: &[&str] = &["BraveSoftware", "Bitwarden"];

// Subdirs of ~/.cache to hide
const CACHE_DENY: &[&str] = &[
    "BraveSoftware",
    "basilisk-dev",
    "chromium",
    "spotify",
    "nvidia",
    "mesa_shader_cache",
];

// ~/.local/share subdirs to mount rw
const LOCAL_SHARE_RW: &[&str] = &[
    "zoxide", "crush", "opencode", "atuin", "mise", "yarn", "flutter", "kotlin", "NuGet",
    "pipx", "ruby-advisory-db", "uv",
];

// ── Entry points for sandbox/mod.rs ────────────────────────────

pub fn check() -> Result<(), String> {
    match Command::new("bwrap").arg("--version").output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(_) => Err("bwrap found but returned an error. Check your installation.".into()),
        Err(_) => Err(
            "bwrap (bubblewrap) not found. Install it:\n  \
             Arch: pacman -S bubblewrap\n  \
             Debian/Ubuntu: apt install bubblewrap\n  \
             Fedora: dnf install bubblewrap"
                .into(),
        ),
    }
}

pub fn prepare() -> Result<SandboxGuard, String> {
    let path = std::env::temp_dir().join(format!("bwrap-hosts.{}", std::process::id()));
    let contents = "127.0.0.1 localhost ai-sandbox\n::1       localhost ai-sandbox\n";
    std::fs::write(&path, contents)
        .map_err(|e| format!("Failed to create temp hosts file: {e}"))?;
    Ok(SandboxGuard { hosts_path: path })
}

pub fn build(guard: &SandboxGuard, config: &Config, project_dir: &Path, verbose: bool) -> Command {
    let mount_set = discover_mounts(config, project_dir, guard.hosts_path(), verbose);
    let mut cmd = Command::new("bwrap");

    for arg in mount_set.all_mount_args() {
        cmd.arg(arg);
    }
    for arg in mount_set.isolation_args(project_dir) {
        cmd.arg(arg);
    }

    let full_cmd = super::build_shell_command(config);
    cmd.arg("bash").arg("-c").arg(&full_cmd);

    cmd
}

pub fn dry_run(guard: &SandboxGuard, config: &Config, project_dir: &Path, verbose: bool) -> String {
    let args = build_dry_run_args(config, project_dir, guard.hosts_path(), verbose);
    format_dry_run_args(&args)
}

// ── Internal helpers ───────────────────────────────────────────

fn build_dry_run_args(
    config: &Config,
    project_dir: &Path,
    hosts_file: &Path,
    verbose: bool,
) -> Vec<String> {
    let mount_set = discover_mounts(config, project_dir, hosts_file, verbose);
    let mut args: Vec<String> = vec!["bwrap".into()];

    args.extend(mount_set.all_mount_args());
    args.extend(mount_set.isolation_args(project_dir));

    let full_cmd = super::build_shell_command(config);
    args.push("bash".into());
    args.push("-c".into());
    args.push(full_cmd);

    args
}

fn format_dry_run_args(args: &[String]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < args.len() {
        if i == 0 {
            out.push_str(&args[0]);
            out.push_str(" \\\n");
            i += 1;
            continue;
        }
        let arg = &args[i];
        if arg.starts_with("--") {
            out.push_str("  ");
            out.push_str(arg);
            // Collect following non-flag args
            let mut j = i + 1;
            while j < args.len() && !args[j].starts_with("--") && args[j] != "bash" {
                out.push(' ');
                let val = &args[j];
                if val.contains(|c: char| c.is_whitespace()) {
                    out.push_str(&format!("'{val}'"));
                } else {
                    out.push_str(val);
                }
                j += 1;
            }
            out.push_str(" \\\n");
            i = j;
        } else {
            // bare args (bash -c ...)
            out.push_str("  ");
            for k in i..args.len() {
                if k > i {
                    out.push(' ');
                }
                let val = &args[k];
                if val.contains(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == '$')
                {
                    out.push_str(&format!("'{val}'"));
                } else {
                    out.push_str(val);
                }
            }
            out.push('\n');
            break;
        }
    }
    out
}

fn discover_mounts(
    config: &Config,
    project_dir: &Path,
    hosts_file: &Path,
    verbose: bool,
) -> MountSet {
    let enable_gpu = config.gpu_enabled();
    let enable_docker = config.docker_enabled();
    let enable_display = config.display_enabled();

    let (display_mounts, display_env) = if enable_display {
        discover_display(verbose)
    } else {
        (vec![], vec![])
    };

    MountSet {
        base: discover_base(hosts_file),
        home_dotfiles: discover_home_dotfiles(verbose),
        config_hide: discover_subdir_hide(".config", CONFIG_DENY),
        cache_hide: discover_subdir_hide(".cache", CACHE_DENY),
        local_overrides: discover_local_overrides(),
        gpu: if enable_gpu {
            discover_gpu(verbose)
        } else {
            vec![]
        },
        docker: if enable_docker {
            discover_docker()
        } else {
            vec![]
        },
        shm: discover_shm(),
        display: display_mounts,
        display_env,
        extra: extra_mounts(&config.rw_maps, &config.ro_maps),
        project: project_mount(project_dir),
    }
}


// ── Mount discovery ────────────────────────────────────────────

fn discover_base(hosts_file: &Path) -> Vec<Mount> {
    vec![
        Mount::RoBind {
            src: "/usr".into(),
            dest: "/usr".into(),
        },
        Mount::Symlink {
            src: "usr/bin".into(),
            dest: "/bin".into(),
        },
        Mount::Symlink {
            src: "usr/lib".into(),
            dest: "/lib".into(),
        },
        Mount::Symlink {
            src: "usr/lib".into(),
            dest: "/lib64".into(),
        },
        Mount::RoBind {
            src: "/etc".into(),
            dest: "/etc".into(),
        },
        Mount::FileRoBind {
            src: hosts_file.to_path_buf(),
            dest: "/etc/hosts".into(),
        },
        Mount::RoBind {
            src: "/opt".into(),
            dest: "/opt".into(),
        },
        Mount::RoBind {
            src: "/sys".into(),
            dest: "/sys".into(),
        },
        Mount::Dev {
            dest: "/dev".into(),
        },
        Mount::Proc {
            dest: "/proc".into(),
        },
        Mount::Tmpfs {
            dest: "/tmp".into(),
        },
        Mount::Tmpfs {
            dest: "/run".into(),
        },
    ]
}

fn discover_home_dotfiles(verbose: bool) -> Vec<Mount> {
    let home = super::home_dir();
    let mut mounts = vec![Mount::Tmpfs { dest: home.clone() }];

    // Discover dot-directories
    let entries = match std::fs::read_dir(&home) {
        Ok(e) => e,
        Err(e) => {
            output::warn(&format!("Cannot read home directory: {e}"));
            return mounts;
        }
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with('.') {
            continue;
        }
        if name_str == "." || name_str == ".." {
            continue;
        }

        let path = entry.path();

        // Only directories
        if !path.is_dir() {
            continue;
        }

        if super::DOTDIR_DENY.contains(&name_str.as_ref()) {
            if verbose {
                output::verbose(&format!("deny: {}", path.display()));
            }
            continue;
        }

        let dest = home.join(&name_str.as_ref());
        if super::DOTDIR_RW.contains(&name_str.as_ref()) {
            if verbose {
                output::verbose(&format!("rw: {}", path.display()));
            }
            mounts.push(Mount::Bind {
                src: path,
                dest,
            });
        } else {
            if verbose {
                output::verbose(&format!("ro: {}", path.display()));
            }
            mounts.push(Mount::RoBind {
                src: path,
                dest,
            });
        }
    }

    // Explicit dotfile mounts (regular files)
    let gitconfig = home.join(".gitconfig");
    if gitconfig.is_file() {
        mounts.push(Mount::RoBind {
            src: gitconfig.clone(),
            dest: gitconfig,
        });
    }
    let claude_json = home.join(".claude.json");
    if claude_json.is_file() {
        mounts.push(Mount::Bind {
            src: claude_json.clone(),
            dest: claude_json,
        });
    }

    mounts
}

fn discover_subdir_hide(parent: &str, deny_list: &[&str]) -> Vec<Mount> {
    let home = super::home_dir();
    deny_list
        .iter()
        .filter_map(|name| {
            let path = home.join(parent).join(name);
            if path.is_dir() {
                Some(Mount::Tmpfs { dest: path })
            } else {
                None
            }
        })
        .collect()
}

fn discover_local_overrides() -> Vec<Mount> {
    let home = super::home_dir();
    let mut mounts = Vec::new();

    let state = home.join(".local/state");
    if state.is_dir() {
        mounts.push(Mount::Bind {
            src: state.clone(),
            dest: state,
        });
    }

    for name in LOCAL_SHARE_RW {
        let path = home.join(".local/share").join(name);
        if path.is_dir() {
            mounts.push(Mount::Bind {
                src: path.clone(),
                dest: path,
            });
        }
    }

    mounts
}

fn discover_gpu(verbose: bool) -> Vec<Mount> {
    let mut mounts = Vec::new();

    // /dev/nvidia*
    if let Ok(entries) = std::fs::read_dir("/dev") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("nvidia") {
                let path = entry.path();
                if verbose {
                    output::verbose(&format!("gpu: {}", path.display()));
                }
                mounts.push(Mount::DevBind {
                    src: path.clone(),
                    dest: path,
                });
            }
        }
    }

    // /dev/dri
    let dri = PathBuf::from("/dev/dri");
    if super::path_exists(&dri) {
        if verbose {
            output::verbose(&format!("gpu: {}", dri.display()));
        }
        mounts.push(Mount::DevBind {
            src: dri.clone(),
            dest: dri,
        });
    }

    mounts
}

fn discover_docker() -> Vec<Mount> {
    let sock = PathBuf::from("/var/run/docker.sock");
    if super::path_exists(&sock) {
        vec![Mount::Bind {
            src: sock.clone(),
            dest: sock,
        }]
    } else {
        vec![]
    }
}

fn discover_shm() -> Vec<Mount> {
    let shm = PathBuf::from("/dev/shm");
    if shm.is_dir() {
        vec![Mount::DevBind {
            src: shm.clone(),
            dest: shm,
        }]
    } else {
        vec![]
    }
}

fn discover_display(verbose: bool) -> (Vec<Mount>, Vec<(String, String)>) {
    let mut mounts = Vec::new();
    let mut env = Vec::new();

    // X11/XWayland socket
    let x11 = PathBuf::from("/tmp/.X11-unix");
    if x11.is_dir() {
        mounts.push(Mount::Bind {
            src: x11.clone(),
            dest: x11,
        });
    }

    if let Ok(display) = std::env::var("DISPLAY") {
        env.push(("DISPLAY".into(), display));
    }

    if let Ok(xauth) = std::env::var("XAUTHORITY") {
        let xauth_path = PathBuf::from(&xauth);
        if super::path_exists(&xauth_path) {
            mounts.push(Mount::RoBind {
                src: xauth_path.clone(),
                dest: xauth_path,
            });
        }
        env.push(("XAUTHORITY".into(), xauth));
    }

    // Wayland / XDG_RUNTIME_DIR
    if let Ok(xdg_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let xdg_path = PathBuf::from(&xdg_dir);
        if xdg_path.is_dir() {
            mounts.push(Mount::Bind {
                src: xdg_path.clone(),
                dest: xdg_path,
            });
            env.push(("XDG_RUNTIME_DIR".into(), xdg_dir));
            if let Ok(wayland) = std::env::var("WAYLAND_DISPLAY") {
                env.push(("WAYLAND_DISPLAY".into(), wayland));
            }
        }
    }

    if verbose {
        for m in &mounts {
            if let Mount::Bind { src, .. } | Mount::RoBind { src, .. } = m {
                output::verbose(&format!("display: {}", src.display()));
            }
        }
    }

    (mounts, env)
}

fn extra_mounts(rw_maps: &[PathBuf], ro_maps: &[PathBuf]) -> Vec<Mount> {
    let mut mounts = Vec::new();

    for path in rw_maps {
        if super::path_exists(path) {
            mounts.push(Mount::Bind {
                src: path.clone(),
                dest: path.clone(),
            });
        } else {
            output::warn(&format!("Path {} not found, skipping.", path.display()));
        }
    }

    for path in ro_maps {
        if super::path_exists(path) {
            mounts.push(Mount::RoBind {
                src: path.clone(),
                dest: path.clone(),
            });
        } else {
            output::warn(&format!("Path {} not found, skipping.", path.display()));
        }
    }

    mounts
}

fn project_mount(project_dir: &Path) -> Vec<Mount> {
    vec![Mount::Bind {
        src: project_dir.to_path_buf(),
        dest: project_dir.to_path_buf(),
    }]
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_test_config() -> Config {
        Config {
            command: vec!["bash".into()],
            no_gpu: Some(true),
            no_docker: Some(true),
            no_display: Some(true),
            no_mise: Some(true),
            ..Config::default()
        }
    }

    // ── Mount::to_args tests ─────────────────────────────────────

    #[test]
    fn mount_args_ro_bind() {
        let m = Mount::RoBind {
            src: "/usr".into(),
            dest: "/usr".into(),
        };
        assert_eq!(m.to_args(), vec!["--ro-bind", "/usr", "/usr"]);
    }

    #[test]
    fn mount_args_bind() {
        let m = Mount::Bind {
            src: "/tmp".into(),
            dest: "/tmp".into(),
        };
        assert_eq!(m.to_args(), vec!["--bind", "/tmp", "/tmp"]);
    }

    #[test]
    fn mount_args_dev_bind() {
        let m = Mount::DevBind {
            src: "/dev/dri".into(),
            dest: "/dev/dri".into(),
        };
        assert_eq!(m.to_args(), vec!["--dev-bind", "/dev/dri", "/dev/dri"]);
    }

    #[test]
    fn mount_args_dev() {
        let m = Mount::Dev {
            dest: "/dev".into(),
        };
        assert_eq!(m.to_args(), vec!["--dev", "/dev"]);
    }

    #[test]
    fn mount_args_proc() {
        let m = Mount::Proc {
            dest: "/proc".into(),
        };
        assert_eq!(m.to_args(), vec!["--proc", "/proc"]);
    }

    #[test]
    fn mount_args_tmpfs() {
        let m = Mount::Tmpfs {
            dest: "/tmp".into(),
        };
        assert_eq!(m.to_args(), vec!["--tmpfs", "/tmp"]);
    }

    #[test]
    fn mount_args_symlink() {
        let m = Mount::Symlink {
            src: "usr/bin".into(),
            dest: "/bin".into(),
        };
        assert_eq!(m.to_args(), vec!["--symlink", "usr/bin", "/bin"]);
    }

    #[test]
    fn mount_args_multiple() {
        let mounts = vec![
            Mount::RoBind {
                src: "/usr".into(),
                dest: "/usr".into(),
            },
            Mount::Tmpfs {
                dest: "/tmp".into(),
            },
        ];
        let args: Vec<String> = mounts.iter().flat_map(|m| m.to_args()).collect();
        assert_eq!(
            args,
            vec!["--ro-bind", "/usr", "/usr", "--tmpfs", "/tmp"]
        );
    }

    #[test]
    fn mount_args_empty() {
        let mounts: Vec<Mount> = vec![];
        let args: Vec<String> = mounts.iter().flat_map(|m| m.to_args()).collect();
        assert!(args.is_empty());
    }

    // ── MountSet::ordered_mounts test ────────────────────────────

    #[test]
    fn ordered_mounts_has_11_groups() {
        let mount_set = MountSet {
            base: vec![],
            gpu: vec![],
            shm: vec![],
            docker: vec![],
            display: vec![],
            home_dotfiles: vec![],
            config_hide: vec![],
            cache_hide: vec![],
            local_overrides: vec![],
            extra: vec![],
            project: vec![],
            display_env: vec![],
        };
        assert_eq!(mount_set.ordered_mounts().len(), 11);
    }

    // ── format_dry_run_args tests (moved from mod.rs) ────────────

    #[test]
    fn format_dry_run_basic() {
        let args: Vec<String> = vec![
            "bwrap".into(),
            "--ro-bind".into(), "/usr".into(), "/usr".into(),
            "--tmpfs".into(), "/tmp".into(),
            "bash".into(), "-c".into(), "true && bash".into(),
        ];
        let output = format_dry_run_args(&args);
        assert!(output.starts_with("bwrap \\\n"));
        assert!(output.contains("--ro-bind /usr /usr"));
        assert!(output.contains("--tmpfs /tmp"));
        assert!(output.contains("bash -c"));
    }

    #[test]
    fn format_dry_run_empty() {
        let args: Vec<String> = vec![];
        let output = format_dry_run_args(&args);
        assert!(output.is_empty());
    }

    // ── dry_run integration tests ───────────────────────────────

    #[test]
    fn dry_run_contains_isolation_flags() {
        let config = minimal_test_config();
        let guard = SandboxGuard::test_with_hosts(PathBuf::from("/tmp/test-hosts"));
        let project = PathBuf::from("/home/user/project");

        let args = build_dry_run_args(&config, &project, guard.hosts_path(), false);

        assert!(args.contains(&"--die-with-parent".to_string()));
        assert!(args.contains(&"--unshare-pid".to_string()));
        assert!(args.contains(&"--unshare-uts".to_string()));
        assert!(args.contains(&"--unshare-ipc".to_string()));
        assert!(args.contains(&"--hostname".to_string()));
        assert!(args.contains(&"ai-sandbox".to_string()));
    }

    #[test]
    fn dry_run_contains_project_dir() {
        let config = minimal_test_config();
        let guard = SandboxGuard::test_with_hosts(PathBuf::from("/tmp/test-hosts"));
        let project = PathBuf::from("/home/user/project");

        let args = build_dry_run_args(&config, &project, guard.hosts_path(), false);

        // Should have --bind /home/user/project /home/user/project
        let project_str = "/home/user/project".to_string();
        let bind_idx = args
            .windows(3)
            .position(|w| w[0] == "--bind" && w[1] == project_str && w[2] == project_str);
        assert!(bind_idx.is_some(), "Project dir should be bound rw");

        // Should have --chdir /home/user/project
        let chdir_idx = args
            .windows(2)
            .position(|w| w[0] == "--chdir" && w[1] == project_str);
        assert!(chdir_idx.is_some(), "Should chdir to project dir");
    }

    #[test]
    fn dry_run_no_gpu_excludes_gpu_mounts() {
        let config = minimal_test_config();
        let guard = SandboxGuard::test_with_hosts(PathBuf::from("/tmp/test-hosts"));
        let project = PathBuf::from("/home/user/project");

        let args = build_dry_run_args(&config, &project, guard.hosts_path(), false);

        let has_gpu_dev_bind = args.windows(3).any(|w| {
            w[0] == "--dev-bind"
                && (w[1].contains("nvidia") || w[1].contains("/dev/dri"))
        });
        assert!(!has_gpu_dev_bind, "GPU disabled: no --dev-bind for GPU devices expected");
    }

    #[test]
    fn dry_run_no_docker_excludes_docker_socket() {
        let config = minimal_test_config();
        let guard = SandboxGuard::test_with_hosts(PathBuf::from("/tmp/test-hosts"));
        let project = PathBuf::from("/home/user/project");

        let args = build_dry_run_args(&config, &project, guard.hosts_path(), false);

        let has_docker = args.iter().any(|a| a.contains("docker.sock"));
        assert!(!has_docker, "Docker disabled: no docker socket expected");
    }

    #[test]
    fn dry_run_no_display_excludes_display_env() {
        let config = minimal_test_config();
        let guard = SandboxGuard::test_with_hosts(PathBuf::from("/tmp/test-hosts"));
        let project = PathBuf::from("/home/user/project");

        let args = build_dry_run_args(&config, &project, guard.hosts_path(), false);

        let display_idx = args
            .windows(3)
            .any(|w| w[0] == "--setenv" && (w[1] == "DISPLAY" || w[1] == "WAYLAND_DISPLAY"));
        assert!(!display_idx, "Display disabled: no DISPLAY env expected");
    }

    #[test]
    fn dry_run_mise_disabled_uses_true_prefix() {
        let mut config = minimal_test_config();
        config.command = vec!["claude".into()];
        let guard = SandboxGuard::test_with_hosts(PathBuf::from("/tmp/test-hosts"));
        let project = PathBuf::from("/home/user/project");

        let args = build_dry_run_args(&config, &project, guard.hosts_path(), false);

        let last = args.last().unwrap();
        assert!(last.starts_with("true && "), "Mise disabled: should use 'true' prefix, got: {last}");
    }

    #[test]
    fn dry_run_default_command_is_bash() {
        let mut config = minimal_test_config();
        config.command = vec![];
        let guard = SandboxGuard::test_with_hosts(PathBuf::from("/tmp/test-hosts"));
        let project = PathBuf::from("/home/user/project");

        let args = build_dry_run_args(&config, &project, guard.hosts_path(), false);
        let last = args.last().unwrap();
        assert!(last.ends_with("bash"), "Default command should be bash, got: {last}");
    }

    #[test]
    fn dry_run_env_vars_present() {
        let config = minimal_test_config();
        let guard = SandboxGuard::test_with_hosts(PathBuf::from("/tmp/test-hosts"));
        let project = PathBuf::from("/home/user/project");

        let args = build_dry_run_args(&config, &project, guard.hosts_path(), false);

        let has_ps1 = args.windows(3).any(|w| w[0] == "--setenv" && w[1] == "PS1");
        assert!(has_ps1, "PS1 env var should be set");

        let has_zo = args.windows(3).any(|w| w[0] == "--setenv" && w[1] == "_ZO_DOCTOR" && w[2] == "0");
        assert!(has_zo, "_ZO_DOCTOR env var should be set to 0");
    }

    #[test]
    fn dry_run_extra_rw_maps_present() {
        let mut config = minimal_test_config();
        config.rw_maps = vec![PathBuf::from("/tmp")];
        let guard = SandboxGuard::test_with_hosts(PathBuf::from("/tmp/test-hosts"));
        let project = PathBuf::from("/home/user/project");

        let args = build_dry_run_args(&config, &project, guard.hosts_path(), false);

        let has_tmp_bind = args
            .windows(3)
            .any(|w| w[0] == "--bind" && w[1] == "/tmp" && w[2] == "/tmp");
        assert!(has_tmp_bind, "Extra rw map /tmp should be present");
    }

    // ── Mount discovery tests ───────────────────────────────────

    #[test]
    fn config_deny_contains_sensitive_apps() {
        assert!(CONFIG_DENY.contains(&"BraveSoftware"));
        assert!(CONFIG_DENY.contains(&"Bitwarden"));
    }

    #[test]
    fn cache_deny_contains_browser_caches() {
        assert!(CACHE_DENY.contains(&"BraveSoftware"));
        assert!(CACHE_DENY.contains(&"chromium"));
    }

    #[test]
    fn base_mounts_has_correct_count() {
        let hosts = PathBuf::from("/tmp/test-hosts");
        let mounts = discover_base(&hosts);
        // /usr, symlink bin, symlink lib, symlink lib64, /etc, hosts, /opt, /sys, /dev, /proc, /tmp, /run
        assert_eq!(mounts.len(), 12);
    }

    #[test]
    fn base_mounts_starts_with_usr() {
        let hosts = PathBuf::from("/tmp/test-hosts");
        let mounts = discover_base(&hosts);
        match &mounts[0] {
            Mount::RoBind { src, dest } => {
                assert_eq!(src, &PathBuf::from("/usr"));
                assert_eq!(dest, &PathBuf::from("/usr"));
            }
            _ => panic!("First mount should be ro-bind /usr"),
        }
    }

    #[test]
    fn base_mounts_includes_hosts_file() {
        let hosts = PathBuf::from("/tmp/my-hosts-file");
        let mounts = discover_base(&hosts);
        let has_hosts = mounts.iter().any(|m| match m {
            Mount::FileRoBind { src, dest } => {
                src == &PathBuf::from("/tmp/my-hosts-file")
                    && dest == &PathBuf::from("/etc/hosts")
            }
            _ => false,
        });
        assert!(has_hosts, "Base mounts should include custom hosts file");
    }

    #[test]
    fn base_mounts_has_symlinks() {
        let hosts = PathBuf::from("/tmp/test-hosts");
        let mounts = discover_base(&hosts);
        let symlinks: Vec<_> = mounts
            .iter()
            .filter_map(|m| match m {
                Mount::Symlink { src, dest } => Some((src.clone(), dest.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(symlinks.len(), 3);
        assert!(symlinks.contains(&("usr/bin".into(), PathBuf::from("/bin"))));
        assert!(symlinks.contains(&("usr/lib".into(), PathBuf::from("/lib"))));
        assert!(symlinks.contains(&("usr/lib".into(), PathBuf::from("/lib64"))));
    }

    #[test]
    fn base_mounts_has_tmpfs_for_tmp_and_run() {
        let hosts = PathBuf::from("/tmp/test-hosts");
        let mounts = discover_base(&hosts);
        let tmpfs_paths: Vec<_> = mounts
            .iter()
            .filter_map(|m| match m {
                Mount::Tmpfs { dest } => Some(dest.clone()),
                _ => None,
            })
            .collect();
        assert!(tmpfs_paths.contains(&PathBuf::from("/tmp")));
        assert!(tmpfs_paths.contains(&PathBuf::from("/run")));
    }

    #[test]
    fn shm_mount_when_exists() {
        let mounts = discover_shm();
        if PathBuf::from("/dev/shm").is_dir() {
            assert_eq!(mounts.len(), 1);
            match &mounts[0] {
                Mount::DevBind { src, dest } => {
                    assert_eq!(src, &PathBuf::from("/dev/shm"));
                    assert_eq!(dest, &PathBuf::from("/dev/shm"));
                }
                _ => panic!("SHM should be dev-bind"),
            }
        } else {
            assert!(mounts.is_empty());
        }
    }

    #[test]
    fn extra_mounts_existing_rw_path() {
        // /tmp always exists
        let mounts = extra_mounts(&[PathBuf::from("/tmp")], &[]);
        assert_eq!(mounts.len(), 1);
        match &mounts[0] {
            Mount::Bind { src, dest } => {
                assert_eq!(src, &PathBuf::from("/tmp"));
                assert_eq!(dest, &PathBuf::from("/tmp"));
            }
            _ => panic!("Expected Bind mount"),
        }
    }

    #[test]
    fn extra_mounts_existing_ro_path() {
        let mounts = extra_mounts(&[], &[PathBuf::from("/tmp")]);
        assert_eq!(mounts.len(), 1);
        match &mounts[0] {
            Mount::RoBind { src, dest } => {
                assert_eq!(src, &PathBuf::from("/tmp"));
                assert_eq!(dest, &PathBuf::from("/tmp"));
            }
            _ => panic!("Expected RoBind mount"),
        }
    }

    #[test]
    fn extra_mounts_nonexistent_path_skipped() {
        let mounts = extra_mounts(
            &[PathBuf::from("/nonexistent/path/that/should/not/exist")],
            &[],
        );
        assert!(mounts.is_empty());
    }

    #[test]
    fn project_mount_is_rw_bind() {
        let mounts = project_mount(Path::new("/home/user/project"));
        assert_eq!(mounts.len(), 1);
        match &mounts[0] {
            Mount::Bind { src, dest } => {
                assert_eq!(src, &PathBuf::from("/home/user/project"));
                assert_eq!(dest, &PathBuf::from("/home/user/project"));
            }
            _ => panic!("Project mount should be a rw bind"),
        }
    }
}
