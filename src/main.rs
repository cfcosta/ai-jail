mod cli;
mod config;
mod mounts;
mod output;
mod sandbox;
mod signals;

use nix::unistd::Pid;
use std::path::PathBuf;

/// RAII guard that removes a temp file on drop.
struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn create_hosts() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("bwrap-hosts.{}", std::process::id()));
        let contents = "127.0.0.1 localhost ai-sandbox\n::1       localhost ai-sandbox\n";
        std::fs::write(&path, contents)
            .map_err(|e| format!("Failed to create temp hosts file: {e}"))?;
        Ok(TempFile { path })
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn run() -> Result<i32, String> {
    let cli = cli::parse()?;

    // Load or skip config
    let existing = if cli.clean {
        config::Config::default()
    } else {
        config::load()
    };

    let config = config::merge(&cli, existing);

    // Handle status command
    if cli.status {
        config::display_status(&config);
        return Ok(0);
    }

    // Handle --init: save config and exit
    if cli.init {
        config::save(&config);
        output::info("Config saved to .ai-jail");
        return Ok(0);
    }

    // Check bwrap is available
    sandbox::check_bwrap()?;

    // Create temp hosts file
    let hosts = TempFile::create_hosts()?;

    let project_dir = std::env::current_dir()
        .map_err(|e| format!("Cannot determine current directory: {e}"))?;

    // Save config (creates .ai-jail on first run, updates on subsequent runs)
    config::save(&config);

    // Handle dry run
    if cli.dry_run {
        let args = sandbox::build_bwrap_dry_run(&config, &project_dir, &hosts.path, cli.verbose);
        let formatted = sandbox::format_dry_run(&args);
        output::dry_run_line(&formatted);
        return Ok(0);
    }

    output::info(&format!("Jail Active: {}", project_dir.display()));

    // Install signal handlers before spawning
    signals::install_handlers();

    // Spawn bwrap
    let mut cmd = sandbox::build_bwrap(&config, &project_dir, &hosts.path, cli.verbose);

    let child = cmd.spawn().map_err(|e| format!("Failed to start bwrap: {e}"))?;

    let pid = child.id() as i32;
    signals::set_child_pid(pid);

    // Wait for child via nix (handles EINTR correctly)
    let exit_code = signals::wait_child(Pid::from_raw(pid));

    // Explicitly forget the std::process::Child since we already waited via nix.
    // This prevents a double-wait.
    std::mem::forget(child);

    Ok(exit_code)
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(msg) => {
            output::error(&msg);
            std::process::exit(1);
        }
    }
}
