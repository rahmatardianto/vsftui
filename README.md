# VSFTUI

**A fast, keyboard-driven Terminal UI and CLI for SFTP/FTP synchronization with `.vscode/sftp.json` compatibility and Git uncommitted auto-sync.**

Built in Rust with `ratatui` and `tokio`, `vsftui` keeps network I/O and SSH handshakes off the UI thread so browsing, queuing transfers, and watching Git changes stay responsive.

---

## Features

- **Drop-in `vscode-sftp` Compatibility**
  - Reads `.vscode/sftp.json`, `.vscode/sftp.jsonc`, or `sftp.json` (JSONC comments and trailing commas supported).
  - Supports single configs, multi-config arrays, and named `profiles` overrides.
  - Expands environment variables (`$SSH_AUTH_SOCK`, `$HOME`) and `~` paths.
  - Supports `syncOption` (`delete`, `skipCreate`, `ignoreExisting`, `update`), `useTempFile` atomic uploads (`.part` + rename), and `filePerm` / `dirPerm`.
- **Git Uncommitted Auto-Sync**
  - Detects staged, modified, untracked, deleted, and renamed files via `libgit2`.
  - One-shot batch sync (`G` in TUI or `--sync-uncommitted` in CLI).
  - Debounced filesystem watcher (`--watch-git` in CLI or automatic in TUI) that also syncs reverted files back to the server.
- **SFTP & FTP with Bastion Hopping**
  - Authenticates via SSH Agent, private key (+ passphrase), or interactive masked password prompt.
  - Supports SSH jump host / bastion proxy (`hop`).
  - Supports FTP (`ftp://`) directory listing, transfers, and `SITE CHMOD`.
- **Parallel Transfer Queue**
  - Concurrent transfers with exponential-backoff retries, live progress bar, speed reporting, and structured activity logs.

---

## Installation

Requires Rust **1.80+** (`cargo` and `rustc`).

```bash
git clone https://github.com/gomppm/vsftui.git
cd vsftui
cargo build --release
```

The compiled binary will be at `target/release/vsftui`. To build with vendored OpenSSL and `libgit2`:

```bash
cargo build --release --features vendored
```

---

## Quick Start

Run `vsftui` inside any project containing `.vscode/sftp.json`:

```bash
# Launch interactive TUI (uses defaultProfile or first profile)
vsftui

# Launch with a specific profile or config file
vsftui --profile production
vsftui --config path/to/sftp.json --workspace /path/to/project
```

### Headless CLI Modes

For scripts, pre-commit hooks, or background daemons:

```bash
# Batch-sync all uncommitted Git changes and exit
vsftui --sync-uncommitted

# Watch workspace and auto-sync Git changes continuously
vsftui --watch-git

# Full workspace sync
vsftui --sync-local-to-remote
vsftui --sync-remote-to-local
```

### CLI Options

| Flag | Description |
| :--- | :--- |
| `-c, --config <PATH>` | Path to `.vscode/sftp.json` |
| `-p, --profile <NAME>` | Profile name to activate |
| `-w, --workspace <DIR>` | Workspace root directory (default: `.`) |
| `--sync-uncommitted` | Batch-sync uncommitted Git changes and exit |
| `--watch-git` | Watch and auto-sync uncommitted Git changes |
| `--sync-local-to-remote` | Sync entire local workspace to remote and exit |
| `--sync-remote-to-local` | Sync entire remote directory to local workspace and exit |

---

## TUI Keybindings

### Navigation

| Key | Action |
| :--- | :--- |
| `Tab` / `Shift+Tab` | Cycle active pane (`Local Workspace` ↔ `Remote Explorer` ↔ `Conveyer & Activity`) |
| `1` / `2` | Switch tabs in the active pane (`Git` / `Local Files`, `Queue` / `Logs`) |
| `j` / `Down`, `k` / `Up` | Move cursor or scroll |
| `Enter` | Open directory, or upload/download selected file |
| `Backspace` / `-` | Go to parent directory |

### Transfers & Actions

| Key | Action |
| :--- | :--- |
| `u` | Upload selected local file or folder |
| `d` | Download selected remote file or folder |
| `s` | Sync selected local or remote folder |
| `G` | Batch-upload all uncommitted Git changes |
| `U` | Sync entire local workspace → remote |
| `D` | Sync entire remote → local workspace |
| `m` | Change octal permissions (`chmod`) on selected local or remote item |

### General

| Key | Action |
| :--- | :--- |
| `p` | Open Profile Switcher |
| `r` | Refresh local files, Git status, and remote directory |
| `c` | Clear completed queue jobs (or clear logs when Logs tab is active) |
| `C` | Clear both completed queue jobs and logs |
| `?` / `Ctrl+h` | Toggle Help modal |
| `q` / `Ctrl+c` | Quit |

---

## Configuration (`.vscode/sftp.json`)

```jsonc
{
  "name": "dev",
  "host": "dev.example.com",
  "protocol": "sftp",
  "port": 22,
  "username": "deploy",
  "privateKeyPath": "~/.ssh/id_ed25519",
  "agent": "$SSH_AUTH_SOCK",
  "remotePath": "/var/www/my-app",
  "useTempFile": true,
  "filePerm": "644",
  "dirPerm": "755",
  "ignore": [
    "**/.git/**",
    "**/.vscode/**",
    "**/node_modules/**",
    "**/target/**"
  ],
  "syncOption": {
    "delete": false,
    "skipCreate": false,
    "ignoreExisting": false,
    "update": true
  },
  "gitAutoSync": {
    "enabled": true,
    "includeUntracked": true,
    "includeStaged": true,
    "includeModified": true,
    "propagateDeletes": false,
    "debounceMs": 300
  },
  "profiles": {
    "staging": {
      "host": "staging.example.com",
      "remotePath": "/var/www/my-app-staging"
    },
    "production": {
      "host": "prod.example.com",
      "remotePath": "/var/www/my-app-production",
      "useTempFile": true
    }
  },
  "defaultProfile": "dev"
}
```

---

## Project Layout

```text
src/
├── main.rs                  # CLI entry point, headless modes, and async TUI loop
├── app.rs                   # Application state, keybindings, and modals
├── config.rs                # JSONC/JSON5 sftp.json parser, profiles, and ignore globs
├── git_sync.rs              # Git index and working tree status inspector
├── watcher.rs               # Debounced filesystem watcher
├── transfer/
│   ├── conveyer.rs          # Parallel transfer queue with retries and job limits
│   └── fs_provider.rs       # RemoteFs implementations (SFTP via ssh2, FTP via suppaftp)
└── ui/
    └── mod.rs               # Ratatui layout, theme, file tables, and modals
```

---

## License

Licensed under the [MIT License](LICENSE).
