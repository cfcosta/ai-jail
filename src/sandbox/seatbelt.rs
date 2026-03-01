use crate::config::Config;
use crate::output;
use std::path::{Path, PathBuf};
use std::process::Command;

// ── SandboxGuard (no resources needed on macOS) ────────────────

pub struct SandboxGuard;

// ── Entry points ───────────────────────────────────────────────

pub fn check() -> Result<(), String> {
    let path = Path::new("/usr/bin/sandbox-exec");
    if path.is_file() {
        Ok(())
    } else {
        Err("sandbox-exec not found at /usr/bin/sandbox-exec. \
             This tool is required for sandboxing on macOS."
            .into())
    }
}

pub fn platform_notes(config: &Config) {
    if !config.gpu_enabled() {
        output::info("--no-gpu has no effect on macOS (Metal is system-level)");
    }
    if !config.display_enabled() {
        output::info("--no-display has no effect on macOS (Cocoa is system-level)");
    }
}

pub fn build(config: &Config, project_dir: &Path, verbose: bool) -> Command {
    let profile = build_profile(config, project_dir, verbose);
    let full_cmd = super::build_shell_command(config);

    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p").arg(&profile);
    cmd.arg("bash").arg("-c").arg(&full_cmd);
    cmd.current_dir(project_dir);

    // Standard env vars
    cmd.env("PS1", "(jail) \\w \\$ ");
    cmd.env("_ZO_DOCTOR", "0");

    cmd
}

pub fn dry_run(config: &Config, project_dir: &Path, verbose: bool) -> String {
    let profile = build_profile(config, project_dir, verbose);
    let full_cmd = super::build_shell_command(config);
    let command_line = format!(
        "sandbox-exec -p '<profile>' bash -c '{full_cmd}'"
    );
    format_dry_run_macos(&command_line, &profile)
}

// ── Internal helpers ───────────────────────────────────────────

fn build_profile(config: &Config, project_dir: &Path, verbose: bool) -> String {
    let profile = generate_sbpl_profile(config, project_dir, config.docker_enabled());

    if verbose {
        output::verbose("SBPL profile:");
        for line in profile.lines() {
            output::verbose(&format!("  {line}"));
        }
    }

    profile
}

fn canonicalize_or_keep(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn generate_sbpl_profile(
    config: &Config,
    project_dir: &Path,
    enable_docker: bool,
) -> String {
    let deny_paths = macos_read_deny_paths();
    let writable_paths = macos_writable_paths(project_dir, config);

    let mut profile = String::new();
    profile.push_str("(version 1)\n");
    profile.push_str("(deny default)\n\n");

    // Process operations
    profile.push_str("; Process operations\n");
    profile.push_str("(allow process-exec)\n");
    profile.push_str("(allow process-fork)\n");
    profile.push_str("(allow signal)\n");
    profile.push_str("(allow sysctl-read)\n\n");

    // IPC and Mach
    profile.push_str("; IPC and Mach\n");
    profile.push_str("(allow mach-lookup)\n");
    profile.push_str("(allow mach-register)\n");
    profile.push_str("(allow ipc-posix-shm-read-data)\n");
    profile.push_str("(allow ipc-posix-shm-write-data)\n");
    profile.push_str("(allow ipc-posix-shm-read-metadata)\n");
    profile.push_str("(allow ipc-posix-shm-write-create)\n\n");

    // Pseudo-terminal operations
    profile.push_str("; Pseudo-terminal\n");
    profile.push_str("(allow pseudo-tty)\n\n");

    // Network
    profile.push_str("; Network\n");
    profile.push_str("(allow network-outbound)\n");
    profile.push_str("(allow network-inbound)\n");
    profile.push_str("(allow network-bind)\n");
    profile.push_str("(allow system-socket)\n\n");

    // File reads: allow globally, then deny sensitive paths
    profile.push_str("; File reads: allow globally, deny sensitive paths\n");
    profile.push_str("(allow file-read*)\n");

    for deny_path in &deny_paths {
        let canonical = canonicalize_or_keep(deny_path);
        let display = canonical.display();
        if canonical.is_dir() {
            profile.push_str(&format!("(deny file-read* (subpath \"{display}\"))\n"));
        } else {
            profile.push_str(&format!("(deny file-read* (literal \"{display}\"))\n"));
        }
    }
    profile.push('\n');

    // File writes: deny by default (from deny default), allow specific paths
    profile.push_str("; File writes: allow specific paths\n");
    for wr_path in &writable_paths {
        let canonical = canonicalize_or_keep(wr_path);
        let display = canonical.display();
        if canonical.is_dir() || !canonical.exists() {
            profile.push_str(&format!("(allow file-write* (subpath \"{display}\"))\n"));
        } else {
            profile.push_str(&format!("(allow file-write* (literal \"{display}\"))\n"));
        }
    }
    profile.push('\n');

    // Docker socket
    if enable_docker {
        if let Some(sock) = macos_docker_socket() {
            let canonical = canonicalize_or_keep(&sock);
            let display = canonical.display();
            profile.push_str("; Docker socket\n");
            profile.push_str(&format!("(allow file-write* (literal \"{display}\"))\n"));
            profile.push('\n');
        }
    }

    profile
}

fn format_dry_run_macos(command_line: &str, profile: &str) -> String {
    let mut out = String::new();
    out.push_str("# sandbox-exec command:\n");
    out.push_str(command_line);
    out.push('\n');
    out.push_str("\n# SBPL profile:\n");
    out.push_str(profile);
    out
}

// ── macOS path discovery ───────────────────────────────────────

fn macos_read_deny_paths() -> Vec<PathBuf> {
    let home = super::home_dir();

    // Shared dotdir denials from mod.rs constants
    let mut candidates: Vec<PathBuf> = super::DOTDIR_DENY
        .iter()
        .map(|name| home.join(name))
        .collect();

    // macOS-specific Library denials
    candidates.extend([
        home.join("Library/Keychains"),
        home.join("Library/Mail"),
        home.join("Library/Messages"),
        home.join("Library/Safari"),
        home.join("Library/Cookies"),
    ]);

    candidates
        .into_iter()
        .filter(|p| super::path_exists(p))
        .collect()
}

fn macos_writable_paths(
    project_dir: &Path,
    config: &Config,
) -> Vec<PathBuf> {
    let home = super::home_dir();
    let mut paths = Vec::new();

    // Project directory
    paths.push(project_dir.to_path_buf());

    // Shared rw dotdirs from mod.rs constants
    for name in super::DOTDIR_RW {
        let p = home.join(name);
        if super::path_exists(&p) {
            paths.push(p);
        }
    }

    // .local is not in DOTDIR_RW (special case on both platforms)
    let local = home.join(".local");
    if super::path_exists(&local) {
        paths.push(local);
    }

    // .claude.json file (rw)
    let claude_json = home.join(".claude.json");
    if claude_json.is_file() {
        paths.push(claude_json);
    }

    // /tmp and /private/tmp
    paths.push(PathBuf::from("/tmp"));
    paths.push(PathBuf::from("/private/tmp"));

    // User rw-maps
    for p in &config.rw_maps {
        if super::path_exists(p) {
            paths.push(p.clone());
        }
    }

    paths
}

fn macos_docker_socket() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from("/var/run/docker.sock"),
        super::home_dir().join(".docker/run/docker.sock"),
    ];
    candidates.into_iter().find(|p| super::path_exists(p))
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sbpl_profile_has_deny_default() {
        let config = Config {
            command: vec!["bash".into()],
            no_mise: Some(true),
            ..Config::default()
        };
        let project = PathBuf::from("/tmp/test-project");
        let profile = generate_sbpl_profile(&config, &project, false);
        assert!(profile.contains("(deny default)"));
    }

    #[test]
    fn sbpl_profile_allows_file_read() {
        let config = Config::default();
        let project = PathBuf::from("/tmp/test-project");
        let profile = generate_sbpl_profile(&config, &project, false);
        assert!(profile.contains("(allow file-read*)"));
    }

    #[test]
    fn sbpl_profile_denies_ssh() {
        let config = Config::default();
        let project = PathBuf::from("/tmp/test-project");
        let profile = generate_sbpl_profile(&config, &project, false);
        let home = super::super::home_dir();
        if home.join(".ssh").exists() {
            assert!(profile.contains(".ssh"));
        }
    }

    #[test]
    fn sbpl_profile_allows_project_write() {
        let config = Config::default();
        let project = PathBuf::from("/tmp/test-project");
        let profile = generate_sbpl_profile(&config, &project, false);
        assert!(profile.contains("file-write*"));
    }

    #[test]
    fn sbpl_profile_allows_network() {
        let config = Config::default();
        let project = PathBuf::from("/tmp/test-project");
        let profile = generate_sbpl_profile(&config, &project, false);
        assert!(profile.contains("(allow network-outbound)"));
        assert!(profile.contains("(allow network-inbound)"));
    }

    #[test]
    fn dry_run_macos_output() {
        let config = Config {
            command: vec!["bash".into()],
            no_mise: Some(true),
            ..Config::default()
        };
        let project = PathBuf::from("/tmp/test-project");
        let output = dry_run(&config, &project, false);
        assert!(output.contains("sandbox-exec"));
        assert!(output.contains("SBPL profile"));
    }

    #[test]
    fn macos_read_deny_includes_ssh() {
        // ~/.ssh almost always exists on dev machines
        let paths = macos_read_deny_paths();
        let has_ssh = paths.iter().any(|p| p.ends_with(".ssh"));
        // Only assert if .ssh exists
        let ssh_dir = super::super::home_dir().join(".ssh");
        if ssh_dir.exists() {
            assert!(has_ssh, "Should deny reads to ~/.ssh");
        }
    }

    #[test]
    fn macos_writable_paths_includes_project() {
        let config = Config::default();
        let project = PathBuf::from("/tmp/test-project");
        let paths = macos_writable_paths(&project, &config);
        assert!(paths.contains(&project), "Project dir must be writable");
    }

    #[test]
    fn macos_writable_paths_includes_tmp() {
        let config = Config::default();
        let project = PathBuf::from("/tmp/test-project");
        let paths = macos_writable_paths(&project, &config);
        assert!(paths.contains(&PathBuf::from("/tmp")));
        assert!(paths.contains(&PathBuf::from("/private/tmp")));
    }

    #[test]
    fn macos_writable_paths_includes_rw_maps() {
        let config = Config {
            rw_maps: vec![PathBuf::from("/tmp")],
            ..Config::default()
        };
        let project = PathBuf::from("/tmp/test-project");
        let paths = macos_writable_paths(&project, &config);
        assert!(paths.contains(&PathBuf::from("/tmp")));
    }

    #[test]
    fn macos_docker_socket_returns_none_when_missing() {
        // This test just verifies the function doesn't panic
        let _ = macos_docker_socket();
    }
}
