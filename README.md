# ai-jail

A bubblewrap (`bwrap`) sandbox for AI coding agents. Isolates tools like Claude Code, GPT Codex, OpenCode, and Crush so they can only access what you explicitly allow.

## Install

### From source

```bash
cargo build --release
cp target/release/ai-jail ~/.local/bin/
```

### Dependencies

- [bubblewrap](https://github.com/containers/bubblewrap) (`bwrap`) must be installed:
  - Arch: `pacman -S bubblewrap`
  - Debian/Ubuntu: `apt install bubblewrap`
  - Fedora: `dnf install bubblewrap`

## Quick Start

```bash
cd ~/Projects/my-app

# Run Claude Code in a sandbox
ai-jail claude

# Run bash inside the sandbox (for debugging)
ai-jail bash

# See what the sandbox would do without running it
ai-jail --dry-run claude
```

On first run, `ai-jail` creates a `.ai-jail` config file in the current directory. Subsequent runs reuse that config. Commit `.ai-jail` to your repo so the sandbox settings follow the project.

## What Gets Sandboxed

### Default behavior (no flags needed)

| Resource | Access | Notes |
|----------|--------|-------|
| `/usr`, `/etc`, `/opt`, `/sys` | read-only | System binaries and config |
| `/dev`, `/proc` | device/proc | Standard device and process access |
| `/tmp`, `/run` | tmpfs | Fresh temp dirs per session |
| `$HOME` | tmpfs | Empty home, then dotfiles layered on top |
| Project directory (pwd) | **read-write** | The whole point |
| GPU devices (`/dev/nvidia*`, `/dev/dri`) | device | For GPU-accelerated tools |
| Docker socket | read-write | If `/var/run/docker.sock` exists |
| X11/Wayland | passthrough | Display server access |
| `/dev/shm` | device | Shared memory (Chromium needs this) |

### Home directory handling

Your real `$HOME` is replaced with a tmpfs. Dotfiles and dotdirs are selectively mounted on top:

**Never mounted (sensitive data):**
- `.gnupg`, `.aws`, `.ssh`, `.mozilla`, `.basilisk-dev`, `.sparrow`

**Mounted read-write (AI tools and build caches):**
- `.claude`, `.crush`, `.codex`, `.aider`, `.config`, `.cargo`, `.cache`, `.docker`

**Everything else:** mounted read-only.

**Additionally hidden (tmpfs over):**
- `~/.config/BraveSoftware`, `~/.config/Bitwarden`
- `~/.cache/BraveSoftware`, `~/.cache/chromium`, `~/.cache/spotify`, `~/.cache/nvidia`, `~/.cache/mesa_shader_cache`, `~/.cache/basilisk-dev`

**Explicit file mounts:**
- `~/.gitconfig` (read-only)
- `~/.claude.json` (read-write)

**Local overrides (read-write):**
- `~/.local/state`
- `~/.local/share/{zoxide,crush,opencode,atuin,mise,yarn,flutter,kotlin,NuGet,pipx,ruby-advisory-db,uv}`

### Namespace isolation

The sandbox uses PID, UTS, and IPC namespace isolation. The hostname inside is `ai-sandbox`. The process dies when the parent exits (`--die-with-parent`).

### mise integration

If [mise](https://mise.jdx.dev/) is found on `$PATH`, the sandbox automatically runs `mise trust && mise activate bash && mise env` before your command. This gives AI tools access to project-specific language versions. Disable with `--no-mise`.

## Usage

```
ai-jail [OPTIONS] [--] [COMMAND [ARGS...]]
```

### Commands

| Command | What it does |
|---------|-------------|
| `claude` | Run Claude Code |
| `codex` | Run GPT Codex |
| `opencode` | Run OpenCode |
| `crush` | Run Crush |
| `bash` | Drop into a bash shell |
| `status` | Show current `.ai-jail` config |
| Any other | Passed through as the command |

If no command is given and no `.ai-jail` config exists, defaults to `bash`.

### Options

| Flag | Description |
|------|-------------|
| `--rw-map <PATH>` | Mount PATH read-write (repeatable) |
| `--map <PATH>` | Mount PATH read-only (repeatable) |
| `--gpu` / `--no-gpu` | Enable/disable GPU passthrough |
| `--docker` / `--no-docker` | Enable/disable Docker socket |
| `--display` / `--no-display` | Enable/disable X11/Wayland |
| `--mise` / `--no-mise` | Enable/disable mise integration |
| `--clean` | Ignore existing config, start fresh |
| `--dry-run` | Print the bwrap command without executing |
| `--init` | Create/update config and exit (don't run) |
| `-v`, `--verbose` | Show detailed mount decisions |
| `-h`, `--help` | Show help |
| `-V`, `--version` | Show version |

### Examples

```bash
# Share an extra library directory read-write
ai-jail --rw-map ~/Projects/shared-lib claude

# Read-only access to reference data
ai-jail --map /opt/datasets claude

# No GPU, no Docker, just the basics
ai-jail --no-gpu --no-docker claude

# See exactly what mounts are being set up
ai-jail --dry-run --verbose claude

# Create config without running
ai-jail --init --no-docker claude

# Regenerate config from scratch
ai-jail --clean --init claude

# Pass flags through to the sub-command (after --)
ai-jail -- claude --model opus
```

## Config File (`.ai-jail`)

Created automatically in the project directory on first run. Example:

```toml
# ai-jail sandbox configuration
# Edit freely. Regenerate with: ai-jail --clean --init

command = ["claude"]
rw_maps = ["/home/user/Projects/shared-lib"]
ro_maps = []
no_gpu = true
```

### Merge behavior

When CLI flags are provided alongside an existing config:

- **command**: CLI replaces config
- **rw_maps / ro_maps**: CLI values are appended (duplicates removed)
- **Boolean flags**: CLI overrides config (`--no-gpu` sets `no_gpu = true`)
- The config file is updated after merge

### Available fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `command` | string array | `["bash"]` | Command to run inside sandbox |
| `rw_maps` | path array | `[]` | Extra read-write mounts |
| `ro_maps` | path array | `[]` | Extra read-only mounts |
| `no_gpu` | bool | not set (auto) | `true` disables GPU passthrough |
| `no_docker` | bool | not set (auto) | `true` disables Docker socket |
| `no_display` | bool | not set (auto) | `true` disables X11/Wayland |
| `no_mise` | bool | not set (auto) | `true` disables mise integration |

When a boolean field is not set, the feature is enabled if the resource exists on the host.

## License

GPL-3.0. See [LICENSE](LICENSE).
