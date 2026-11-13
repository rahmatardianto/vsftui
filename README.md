# ⚡ VSFTUI: High-Performance Rust SFTP/FTP TUI with Git Uncommitted Auto-Sync

`vsftui` is a modern Terminal User Interface (TUI) built in **Rust**, designed as a robust, *drop-in compatible* replacement for the Visual Studio Code extension `Natizyskunk/vscode-sftp` (and its original fork `liximomo/vscode-sftp`), featuring first-class **Git Uncommitted Auto-Sync**.

---

## 🌟 Key Features

1. **Drop-in `.vscode/sftp.json` Compatibility (JSONC & JSON5)**:
   - Supports `//` and `/* */` comments as well as *trailing commas*.
   - Supports Single Config, Multi-Config Array, and Named Profiles (`"profiles": { "dev": {...}, "prod": {...} }`).
   - Automatic environment variable expansion (`$SSH_AUTH_SOCK`, `$HOME`, and tilde `~`).
   - Supports SFTP (SSH key/passphrase, agent, password) and plain FTP connections.
   - Supports SSH Jump Host / Bastion Proxy Hopping (`hop`).
2. **First-Class Git Uncommitted Auto-Sync**:
   - Detects uncommitted local file changes in real time via `git2` (*libgit2*).
   - Marks file statuses: `[M]` (Modified), `[A]` (Staged New), `[??]` (Untracked), `[D]` (Deleted), `[R]` (Renamed).
   - Batch Sync mode (`Shift+G` in TUI or `--sync-uncommitted` via CLI).
   - Continuous Watch Auto-Sync mode (`g` toggle in TUI or `--watch-git` via CLI) with automatic *debouncing*.
   - Automatically syncs reverted/restored files (`git restore <file>` / `git checkout -- <file>`) to maintain remote server integrity.
3. **Zero-Blocking Asynchronous Architecture (Tokio + Dedicated SFTP Worker)**:
   - Network I/O, SSH handshakes, and directory listings run on isolated workers. The `ratatui` UI thread never *freezes* or *stutters*.
   - Parallel transfer queue (*Conveyer*) with concurrency control (`tokio::sync::Semaphore`), transfer speed reporting (KB/s, MB/s), and automatic retries with *exponential backoff*.
   - Atomic transfers: when `"useTempFile": true`, files are uploaded to `<remote>.part` first and atomically renamed upon completion.
4. **Credential Security**:
   - Passwords and passphrases are never stored in plaintext logs.
   - Supports SSH Agent (`$SSH_AUTH_SOCK`) and a masked (`*`) interactive password prompt modal.

---

## 📂 Project Structure

```text
vsftui/
├── Cargo.toml                  # Dependency manifest & version pins
├── README.md                   # Complete documentation & usage guide
├── .vscode/
│   └── sftp.json               # Example vscode-sftp compatible configuration
└── src/
    ├── main.rs                 # CLI entry point (clap), signal handling & TUI bootstrap
    ├── app.rs                  # Application state machine, event dispatcher, tabs & modals
    ├── config.rs               # JSONC/JSON5 sftp.json parser, profile resolver & globs
    ├── git_sync.rs             # Git working tree & index status inspection engine
    ├── watcher.rs              # Debounced filesystem watcher (notify-debouncer-mini)
    ├── transfer/
    │   ├── mod.rs              # Transfer module
    │   ├── conveyer.rs         # Parallel transfer queue (Semaphore, retry, progress)
    │   └── fs_provider.rs      # RemoteFs abstraction (SFTP via ssh2 & FTP via suppaftp)
    └── ui/
        └── mod.rs              # Responsive 4-zone ratatui layout & modal popups
```

---

## 🛠️ Installation & Build

Ensure a Rust toolchain (`cargo` & `rustc`) version 1.80+ or stable is installed (e.g. via `mise` or `rustup`):

```bash
# Clone or enter the repository
cd vsftui

# Run unit tests
cargo test

# Build release binary
cargo build --release
```

---

## 🚀 Usage Guide

### 1. Interactive Terminal UI (TUI) Mode
Run inside a project directory containing `.vscode/sftp.json`:
```bash
cargo run
# or specify a profile:
cargo run -- --profile production
```

### 2. Headless Mode: Batch Sync Git Changes (One-Shot)
Ideal for quick deployment scripts, pre-commit hooks, or CI/CD:
```bash
cargo run -- --sync-uncommitted
```

### 3. Headless Mode: Continuous Git Watcher Daemon
Watches the local working directory and automatically uploads uncommitted Git changes to the server:
```bash
cargo run -- --watch-git
```

---

## ⌨️ TUI Keybindings

| Key | Action |
| :--- | :--- |
| `Tab` | Cycle active pane focus (`Left Pane` ➔ `Right Pane` ➔ `Bottom Pane`) |
| `1` / `2` | Switch tabs in the active pane (`Git Uncommitted` / `Local Explorer`, `Queue` / `Logs`) |
| `j` / `Down` | Move cursor down in list / table |
| `k` / `Up` | Move cursor up in list / table |
| `Enter` | Enter subdirectory / upload selected local file / download selected remote file |
| `Backspace` / `-` | Go up to parent directory |
| `u` | Upload selected local file or directory to server |
| `d` | Download selected remote file or directory to local |
| `g` | Toggle Continuous Git Uncommitted Auto-Sync (`ON` / `OFF`) |
| `G` (*Shift+G*) | Trigger Batch Sync for all current uncommitted files |
| `p` | Open server Profile Switcher modal (`sftp.json` profiles) |
| `r` | Refresh local directory, remote directory, and Git status |
| `c` | Clear completed transfer tasks from the queue |
| `?` / `h` | Open Help guide modal |
| `q` / `Ctrl+C` | Exit application safely (*graceful shutdown*) |

---

## ⚙️ Example `.vscode/sftp.json` Configuration

```jsonc
// .vscode/sftp.json
{
  "name": "Development Server",
  "host": "dev.example.com",
  "protocol": "sftp",
  "port": 22,
  "username": "deploy",
  "privateKeyPath": "~/.ssh/id_ed25519",
  "agent": "$SSH_AUTH_SOCK",
  "remotePath": "/var/www/my-app",
  "uploadOnSave": true,
  "useTempFile": true,
  "ignore": [
    "**/.git/**",
    "**/.vscode/**",
    "**/node_modules/**",
    "**/target/**",
  ],
  // Extended Feature: Git Uncommitted Auto-Sync
  "gitAutoSync": {
    "enabled": true,
    "includeUntracked": true,
    "includeStaged": true,
    "includeModified": true,
    "propagateDeletes": false,
    "debounceMs": 300,
  },
  "profiles": {
    "staging": {
      "host": "staging.example.com",
      "remotePath": "/var/www/my-app-staging",
    },
    "production": {
      "host": "prod.example.com",
      "remotePath": "/var/www/my-app-production",
      "useTempFile": true,
    },
  },
}
```
