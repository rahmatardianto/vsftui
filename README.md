# ⚡ SFTUI: High-Performance Rust SFTP/FTP TUI with Git Uncommitted Auto-Sync

`sftui` adalah Terminal User Interface (TUI) modern berbasis **Rust** yang dirancang sebagai pengganti tangguh dan *drop-in compatible* untuk ekstensi Visual Studio Code `Natizyskunk/vscode-sftp` (dan fork aslinya `liximomo/vscode-sftp`), dilengkapi fitur unggulan **Git Uncommitted Auto-Sync**.

---

## 🌟 Fitur Utama

1. **Drop-in Compatibility `.vscode/sftp.json` (JSONC & JSON5)**:
   - Mendukung komentar `//`, `/* */`, dan *trailing comma*.
   - Mendukung format Single Config, Multi-Config Array, serta Named Profiles (`"profiles": { "dev": {...}, "prod": {...} }`).
   - Ekspansi otomatis environment variable (`$SSH_AUTH_SOCK`, `$HOME`, dan tilde `~`).
   - Mendukung koneksi SFTP (SSH key/passphrase, agent, password) dan FTP murni.
   - Mendukung SSH Jump Host / Bastion Proxy Hopping (`hop`).
2. **First-Class Git Uncommitted Auto-Sync**:
   - Mendeteksi perubahan file lokal yang belum di-commit secara real-time via `git2` (*libgit2*).
   - Menandai status file: `[M]` (Modified), `[A]` (Staged New), `[??]` (Untracked), `[D]` (Deleted), `[R]` (Renamed).
   - Mode Batch Sync (`Shift+G` di TUI atau `--sync-uncommitted` via CLI).
   - Mode Continuous Watch Auto-Sync (`g` toggle di TUI atau `--watch-git` via CLI) dengan *debouncing* otomatis.
   - Sinkronisasi otomatis saat file di-revert/restore (`git restore <file>` / `git checkout -- <file>`) untuk menjaga integritas remote server.
3. **Arsitektur Asinkron Zero-Blocking (Tokio + Dedicated SFTP Worker)**:
   - Operasi I/O jaringan, SSH handshake, dan pembacaan direktori berjalan di worker terisolasi. UI thread `ratatui` tidak pernah *freeze* atau *stutter*.
   - Queue transfer paralel (*Conveyer*) dengan kontrol konkurensi (`tokio::sync::Semaphore`), pelaporan kecepatan transfer (KB/s, MB/s), retry otomatis dengan *exponential backoff*.
   - Transfer atomik: jika `"useTempFile": true`, file diunggah ke `<remote>.part` terlebih dahulu dan di-rename secara atomik setelah selesai.
4. **Keamanan Kredensial**:
   - Password dan passphrase tidak pernah disimpan dalam plaintext log.
   - Dukungan SSH Agent (`$SSH_AUTH_SOCK`) dan modal password interaktif bertopeng (`*`).

---

## 📂 Struktur Proyek

```text
sftui/
├── Cargo.toml                  # Manifest dependensi & pin versi
├── README.md                   # Dokumentasi lengkap & panduan penggunaan
├── .vscode/
│   └── sftp.json              # Contoh konfigurasi kompatibel dengan vscode-sftp
└── src/
    ├── main.rs                 # CLI entry point (clap), signal handling & TUI bootstrap
    ├── app.rs                  # State machine aplikasi, event dispatcher, tabs & modals
    ├── config.rs               # Parser JSONC/JSON5 sftp.json, resolver profile & globs
    ├── git_sync.rs             # Engine pemeriksa status Git working tree & index
    ├── watcher.rs              # Filesystem debounced watcher (notify-debouncer-mini)
    ├── transfer/
    │   ├── mod.rs              # Modul transfer
    │   ├── conveyer.rs         # Queue transfer paralel (Semaphore, retry, progress)
    │   └── fs_provider.rs      # Abstraksi RemoteFs (SFTP via ssh2 & FTP via suppaftp)
    └── ui/
        └── mod.rs              # Layout 4 zona responsif ratatui & modal popups
```

---

## 🛠️ Instalasi & Kompilasi

Pastikan toolchain Rust (`cargo` & `rustc`) versi 1.80+ atau stabil telah terinstal (misal via `mise` atau `rustup`):

```bash
# Clone atau masuk ke repositori
cd /home/arch/Projects/sftui

# Jalankan pengujian unit
cargo test

# Kompilasi rilis
cargo build --release
```

---

## 🚀 Panduan Penggunaan

### 1. Mode Interaktif Terminal UI (TUI)
Jalankan di direktori proyek yang memiliki `.vscode/sftp.json`:
```bash
cargo run
# atau gunakan profil tertentu:
cargo run -- --profile production
```

### 2. Mode Headless: Batch Sync Perubahan Git (Sekali Jalan)
Sangat cocok untuk script deployment cepat, pre-commit hook, atau CI/CD:
```bash
cargo run -- --sync-uncommitted
```

### 3. Mode Headless: Continuous Git Watcher Daemon
Memantau direktori kerja lokal dan secara otomatis mengunggah perubahan Git yang belum di-commit ke server:
```bash
cargo run -- --watch-git
```

---

## ⌨️ Pintasan Tombol TUI (Keybindings)

| Tombol | Aksi |
| :--- | :--- |
| `Tab` | Pindah fokus panel aktif (`Left Pane` ➔ `Right Pane` ➔ `Bottom Pane`) |
| `1` / `2` | Beralih tab pada panel yang sedang aktif (`Git Uncommitted` / `Local Explorer`, `Queue` / `Logs`) |
| `j` / `Down` | Geser kursor ke bawah pada daftar / tabel |
| `k` / `Up` | Geser kursor ke atas pada daftar / tabel |
| `Enter` | Masuk ke subdirektori / unggah file terpilih / unduh file remote terpilih |
| `Backspace` / `-` | Kembali ke direktori induk (*parent folder*) |
| `u` | Unggah file atau direktori lokal terpilih ke server |
| `d` | Unduh file atau direktori remote terpilih ke lokal |
| `g` | Toggle Continuous Git Uncommitted Auto-Sync (`ON` / `OFF`) |
| `G` (*Shift+G*) | Memicu Batch Sync untuk semua file uncommitted saat ini |
| `p` | Buka modal Switcher Profil server (`sftp.json` profiles) |
| `r` | Refresh status direktori lokal, direktori remote, dan status Git |
| `c` | Bersihkan riwayat tugas transfer yang telah selesai dari queue |
| `?` / `h` | Buka modal panduan bantuan (*Help*) |
| `q` / `Ctrl+C` | Keluar dari aplikasi dengan aman (*graceful shutdown*) |

---

## ⚙️ Contoh Konfigurasi `.vscode/sftp.json`

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
  // Ekstensi Baru: Git Uncommitted Auto-Sync
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
