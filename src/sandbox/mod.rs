use crate::config::Config;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "linux")]
mod bwrap;
#[cfg(target_os = "macos")]
mod seatbelt;

// ── Re-export platform SandboxGuard ────────────────────────────

#[cfg(target_os = "linux")]
pub use bwrap::SandboxGuard;
#[cfg(target_os = "macos")]
pub use seatbelt::SandboxGuard;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub struct SandboxGuard;

// ── Shared constants ───────────────────────────────────────────

// Dotdirs never mounted (sensitive data)
const DOTDIR_DENY: &[&str] = &[".gnupg", ".aws", ".ssh", ".mozilla", ".basilisk-dev", ".sparrow"];

// Dotdirs requiring read-write access
const DOTDIR_RW: &[&str] = &[
    ".claude", ".crush", ".codex", ".aider", ".config", ".cargo", ".cache", ".docker",
];

// ── Shared utilities ───────────────────────────────────────────

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

fn path_exists(p: &Path) -> bool {
    p.exists() || p.symlink_metadata().is_ok()
}

fn mise_bin() -> Option<PathBuf> {
    std::env::var("PATH")
        .ok()
        .and_then(|paths| {
            paths.split(':').find_map(|dir| {
                let p = PathBuf::from(dir).join("mise");
                if p.is_file() {
                    Some(p)
                } else {
                    None
                }
            })
        })
}

fn mise_init_cmd(mise_path: &Path) -> String {
    let p = mise_path.display();
    format!("{p} trust && eval \"$({p} activate bash)\" && eval \"$({p} env)\"")
}

fn shell_join(parts: &[String]) -> String {
    parts
        .iter()
        .map(|s| {
            if s.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == '\\') {
                format!("'{}'", s.replace('\'', "'\\''"))
            } else {
                s.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn build_shell_command(config: &Config) -> String {
    let use_mise = config.mise_enabled();
    let mise_prefix = if use_mise {
        mise_bin().map(|p| mise_init_cmd(&p))
    } else {
        None
    };
    let mise_prefix = mise_prefix.as_deref().unwrap_or("true");

    let user_cmd = if config.command.is_empty() {
        "bash".to_string()
    } else {
        shell_join(&config.command)
    };

    format!("{mise_prefix} && {user_cmd}")
}

// ── Platform dispatchers ───────────────────────────────────────

pub fn check() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        return bwrap::check();
    }
    #[cfg(target_os = "macos")]
    {
        return seatbelt::check();
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err("ai-jail is only supported on Linux and macOS".into())
    }
}

pub fn prepare() -> Result<SandboxGuard, String> {
    #[cfg(target_os = "linux")]
    {
        return bwrap::prepare();
    }
    #[cfg(target_os = "macos")]
    {
        return Ok(seatbelt::SandboxGuard);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Ok(SandboxGuard)
    }
}

pub fn platform_notes(config: &Config) {
    #[cfg(target_os = "macos")]
    {
        seatbelt::platform_notes(config);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
    }
}

pub fn build(guard: &SandboxGuard, config: &Config, project_dir: &Path, verbose: bool) -> Command {
    #[cfg(target_os = "linux")]
    {
        return bwrap::build(guard, config, project_dir, verbose);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = guard;
        return seatbelt::build(config, project_dir, verbose);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (guard, config, project_dir, verbose);
        unreachable!("check() prevents reaching here on unsupported platforms")
    }
}

pub fn dry_run(guard: &SandboxGuard, config: &Config, project_dir: &Path, verbose: bool) -> String {
    #[cfg(target_os = "linux")]
    {
        return bwrap::dry_run(guard, config, project_dir, verbose);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = guard;
        return seatbelt::dry_run(config, project_dir, verbose);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (guard, config, project_dir, verbose);
        unreachable!("check() prevents reaching here on unsupported platforms")
    }
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── shell_join tests ────────────────────────────────────────

    #[test]
    fn shell_join_simple() {
        let parts = vec!["claude".to_string()];
        assert_eq!(shell_join(&parts), "claude");
    }

    #[test]
    fn shell_join_multiple_words() {
        let parts = vec!["claude".into(), "--model".into(), "opus".into()];
        assert_eq!(shell_join(&parts), "claude --model opus");
    }

    #[test]
    fn shell_join_with_spaces() {
        let parts = vec!["echo".into(), "hello world".into()];
        assert_eq!(shell_join(&parts), "echo 'hello world'");
    }

    #[test]
    fn shell_join_with_single_quotes() {
        let parts = vec!["echo".into(), "it's".into()];
        assert_eq!(shell_join(&parts), "echo 'it'\\''s'");
    }

    #[test]
    fn shell_join_empty() {
        let parts: Vec<String> = vec![];
        assert_eq!(shell_join(&parts), "");
    }

    // ── mise_init_cmd tests ─────────────────────────────────────

    #[test]
    fn mise_init_cmd_format() {
        let cmd = mise_init_cmd(Path::new("/usr/bin/mise"));
        assert!(cmd.contains("/usr/bin/mise trust"));
        assert!(cmd.contains("/usr/bin/mise activate bash"));
        assert!(cmd.contains("/usr/bin/mise env"));
    }

    // ── build_shell_command tests ───────────────────────────────

    #[test]
    fn build_shell_command_default_is_bash() {
        let config = Config {
            no_mise: Some(true),
            ..Config::default()
        };
        let cmd = build_shell_command(&config);
        assert_eq!(cmd, "true && bash");
    }

    #[test]
    fn build_shell_command_with_command() {
        let config = Config {
            command: vec!["claude".into()],
            no_mise: Some(true),
            ..Config::default()
        };
        let cmd = build_shell_command(&config);
        assert_eq!(cmd, "true && claude");
    }

    // ── Deny/RW list tests ──────────────────────────────────────

    #[test]
    fn deny_list_contains_sensitive_dirs() {
        for name in &[".gnupg", ".aws", ".ssh", ".mozilla", ".basilisk-dev", ".sparrow"] {
            assert!(
                DOTDIR_DENY.contains(name),
                "{name} should be in deny list"
            );
        }
    }

    #[test]
    fn rw_list_contains_ai_tool_dirs() {
        for name in &[".claude", ".crush", ".codex", ".aider"] {
            assert!(
                DOTDIR_RW.contains(name),
                "{name} should be in rw list"
            );
        }
    }

    #[test]
    fn rw_list_contains_tool_dirs() {
        for name in &[".config", ".cargo", ".cache", ".docker"] {
            assert!(
                DOTDIR_RW.contains(name),
                "{name} should be in rw list"
            );
        }
    }

    #[test]
    fn deny_and_rw_lists_do_not_overlap() {
        for name in DOTDIR_DENY {
            assert!(
                !DOTDIR_RW.contains(name),
                "{name} is in both deny and rw lists"
            );
        }
    }
}
