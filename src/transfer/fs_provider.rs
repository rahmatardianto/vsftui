use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use ssh2::{OpenFlags, OpenType, RenameFlags, Session, Sftp};
use suppaftp::tokio::AsyncFtpStream;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::config::{HopConfig, SftpConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
    pub permissions: Option<u32>,
}

#[allow(dead_code)]
#[async_trait]
pub trait RemoteFs: Send + Sync {
    async fn stat(&self, remote_path: &str) -> Result<FileStat>;
    async fn read_dir(&self, remote_path: &str) -> Result<Vec<FileStat>>;
    async fn upload(&self, local_path: &Path, remote_path: &str, use_temp_file: bool) -> Result<u64>;
    async fn download(&self, remote_path: &str, local_path: &Path) -> Result<u64>;
    async fn remove_file(&self, remote_path: &str) -> Result<()>;
    async fn remove_dir(&self, remote_path: &str) -> Result<()>;
    async fn mkdir_all(&self, remote_path: &str) -> Result<()>;
    async fn chmod(&self, remote_path: &str, mode: u32) -> Result<()>;
    async fn disconnect(&self) -> Result<()>;
}

// -----------------------------------------------------------------------------
// SFTP Implementation via dedicated worker thread
// -----------------------------------------------------------------------------

#[allow(dead_code)]
enum SftpCommand {
    Stat {
        path: String,
        reply: oneshot::Sender<Result<FileStat>>,
    },
    ReadDir {
        path: String,
        reply: oneshot::Sender<Result<Vec<FileStat>>>,
    },
    Upload {
        local_path: PathBuf,
        remote_path: String,
        use_temp_file: bool,
        reply: oneshot::Sender<Result<u64>>,
    },
    Download {
        remote_path: String,
        local_path: PathBuf,
        reply: oneshot::Sender<Result<u64>>,
    },
    RemoveFile {
        path: String,
        reply: oneshot::Sender<Result<()>>,
    },
    RemoveDir {
        path: String,
        reply: oneshot::Sender<Result<()>>,
    },
    MkdirAll {
        path: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Chmod {
        path: String,
        mode: u32,
        reply: oneshot::Sender<Result<()>>,
    },
    Disconnect {
        reply: oneshot::Sender<Result<()>>,
    },
}

pub struct SftpBackend {
    tx: mpsc::Sender<SftpCommand>,
}

impl SftpBackend {
    pub async fn connect(config: SftpConfig) -> Result<Self> {
        let (tx, mut rx) = mpsc::channel::<SftpCommand>(64);
        let (init_tx, init_rx) = oneshot::channel::<Result<()>>();

        // Spawn dedicated worker thread for ssh2 (ensures Session remains on single OS thread)
        std::thread::Builder::new()
            .name("sftp-worker".to_string())
            .spawn(move || {
                let conn_res = Self::init_session_and_sftp(&config);
                let (session, sftp) = match conn_res {
                    Ok((sess, sftp)) => {
                        let _ = init_tx.send(Ok(()));
                        (sess, sftp)
                    }
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };

                let mut worker = SftpWorker {
                    _session: session,
                    sftp,
                    file_perm: config.file_perm,
                    dir_perm: config.dir_perm,
                };

                while let Some(cmd) = rx.blocking_recv() {
                    match cmd {
                        SftpCommand::Stat { path, reply } => {
                            let _ = reply.send(worker.stat(&path));
                        }
                        SftpCommand::ReadDir { path, reply } => {
                            let _ = reply.send(worker.read_dir(&path));
                        }
                        SftpCommand::Upload {
                            local_path,
                            remote_path,
                            use_temp_file,
                            reply,
                        } => {
                            let _ = reply.send(worker.upload(&local_path, &remote_path, use_temp_file));
                        }
                        SftpCommand::Download {
                            remote_path,
                            local_path,
                            reply,
                        } => {
                            let _ = reply.send(worker.download(&remote_path, &local_path));
                        }
                        SftpCommand::RemoveFile { path, reply } => {
                            let _ = reply.send(worker.remove_file(&path));
                        }
                        SftpCommand::RemoveDir { path, reply } => {
                            let _ = reply.send(worker.remove_dir(&path));
                        }
                        SftpCommand::MkdirAll { path, reply } => {
                            let _ = reply.send(worker.mkdir_all(&path));
                        }
                        SftpCommand::Chmod { path, mode, reply } => {
                            let _ = reply.send(worker.chmod(&path, mode));
                        }
                        SftpCommand::Disconnect { reply } => {
                            let _ = reply.send(Ok(()));
                            break;
                        }
                    }
                }
            })
            .context("Failed to spawn SFTP worker thread")?;

        init_rx.await.context("SFTP worker channel dropped unexpectedly")??;

        Ok(Self { tx })
    }

    fn init_session_and_sftp(config: &SftpConfig) -> Result<(Session, Sftp)> {
        let session = if let Some(ref hop) = config.hop {
            Self::connect_via_hop(hop, &config.host, config.port, config)?
        } else {
            Self::connect_direct(&config.host, config.port, config)?
        };

        let sftp = session
            .sftp()
            .context("Failed to initialize SFTP subsystem")?;

        Ok((session, sftp))
    }

    fn connect_direct(host: &str, port: u16, config: &SftpConfig) -> Result<Session> {
        let addr = format!("{}:{}", host, port);
        let stream = TcpStream::connect_timeout(
            &addr
                .parse()
                .or_else(|_| {
                    use std::net::ToSocketAddrs;
                    addr.to_socket_addrs()?
                        .next()
                        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "Host address resolution failed"))
                })?,
            Duration::from_secs(10),
        )
        .with_context(|| format!("Failed to connect TCP to {}", addr))?;

        let mut session = Session::new().context("Failed to create SSH session")?;
        session.set_timeout(15_000);
        session.set_tcp_stream(stream);
        session.handshake().context("SSH handshake failed")?;

        Self::authenticate_session(&mut session, &config.username, config)?;
        Ok(session)
    }

    fn connect_via_hop(
        hop: &HopConfig,
        target_host: &str,
        target_port: u16,
        target_config: &SftpConfig,
    ) -> Result<Session> {
        let hop_addr = format!("{}:{}", hop.host, hop.port);
        let hop_stream = TcpStream::connect_timeout(
            &hop_addr
                .parse()
                .or_else(|_| {
                    use std::net::ToSocketAddrs;
                    hop_addr
                        .to_socket_addrs()?
                        .next()
                        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "Hop address resolution failed"))
                })?,
            Duration::from_secs(10),
        )
        .with_context(|| format!("Failed to connect TCP to Hop Host {}", hop_addr))?;

        let mut hop_session = Session::new().context("Failed to create SSH session for hop")?;
        hop_session.set_timeout(15_000);
        hop_session.set_tcp_stream(hop_stream);
        hop_session.handshake().context("SSH hop handshake failed")?;

        // Authenticate hop
        if hop.agent.is_some() {
            if let Ok(mut agent) = hop_session.agent() {
                let _ = agent.connect();
                let _ = agent.list_identities();
                for id in agent.identities().unwrap_or_default() {
                    if agent.userauth(&hop.username, &id).is_ok() {
                        break;
                    }
                }
            }
        }
        if !hop_session.authenticated() {
            if let Some(ref key_path) = hop.private_key_path {
                let path = Path::new(key_path);
                if path.exists() {
                    let _ = hop_session.userauth_pubkey_file(
                        &hop.username,
                        None,
                        path,
                        hop.passphrase.as_deref(),
                    );
                }
            }
        }
        if !hop_session.authenticated() {
            if let Some(ref pass) = hop.password {
                let _ = hop_session.userauth_password(&hop.username, pass);
            }
        }

        if !hop_session.authenticated() {
            return Err(anyhow!("Failed to authenticate with SSH hop bastion"));
        }

        // Open direct-tcpip channel to target host
        let mut channel = hop_session
            .channel_direct_tcpip(target_host, target_port, None)
            .with_context(|| format!("Failed to establish direct-tcpip channel through hop to {}:{}", target_host, target_port))?;

        // Create local loopback bridge for libssh2 target session
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .context("Failed to bind local loopback listener for SSH hop bridge")?;
        let local_addr = listener.local_addr()?;
        hop_session.set_blocking(false);

        std::thread::Builder::new()
            .name("ssh-hop-bridge".to_string())
            .spawn(move || {
                if let Ok((mut socket, _)) = listener.accept() {
                    let _ = socket.set_nonblocking(true);
                    let mut s2c = [0u8; 16384];
                    let mut c2s = [0u8; 16384];
                    loop {
                        let mut idle = true;
                        match socket.read(&mut s2c) {
                            Ok(n) if n > 0 => {
                                if channel.write_all(&s2c[..n]).is_err() {
                                    break;
                                }
                                idle = false;
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                            _ => break,
                        }
                        match channel.read(&mut c2s) {
                            Ok(n) if n > 0 => {
                                if socket.write_all(&c2s[..n]).is_err() {
                                    break;
                                }
                                idle = false;
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                            _ => break,
                        }
                        if idle {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    }
                }
            })
            .context("Failed to spawn SSH hop bridge thread")?;

        let target_stream = TcpStream::connect_timeout(&local_addr, Duration::from_secs(5))
            .with_context(|| "Failed to connect to local SSH hop bridge")?;

        let mut target_session = Session::new().context("Failed to create SSH target session")?;
        target_session.set_timeout(15_000);
        target_session.set_tcp_stream(target_stream);
        target_session.handshake().context("Target SSH handshake through hop failed")?;

        Self::authenticate_session(&mut target_session, &target_config.username, target_config)?;
        Ok(target_session)
    }

    fn authenticate_session(session: &mut Session, username: &str, config: &SftpConfig) -> Result<()> {
        // 1. Try SSH Agent if configured or $SSH_AUTH_SOCK exists
        let use_agent = config.agent.is_some() || std::env::var("SSH_AUTH_SOCK").is_ok();
        if use_agent {
            if let Ok(mut agent) = session.agent() {
                if agent.connect().is_ok() && agent.list_identities().is_ok() {
                    for identity in agent.identities().unwrap_or_default() {
                        if agent.userauth(username, &identity).is_ok() {
                            tracing::info!("Authenticated using SSH Agent");
                            return Ok(());
                        }
                    }
                }
            }
        }

        // 2. Try Private Key
        if let Some(ref key_path) = config.private_key_path {
            let path = Path::new(key_path);
            if path.exists() {
                session
                    .userauth_pubkey_file(username, None, path, config.passphrase.as_deref())
                    .with_context(|| format!("Private key authentication failed for {}", key_path))?;
                if session.authenticated() {
                    tracing::info!("Authenticated using private key: {}", key_path);
                    return Ok(());
                }
            }
        }

        // 3. Try Password
        if let Some(ref password) = config.password {
            session
                .userauth_password(username, password)
                .context("Password authentication failed")?;
            if session.authenticated() {
                tracing::info!("Authenticated using password");
                return Ok(());
            }
        }

        // 4. Default userauth_agent
        if let Ok(mut agent) = session.agent() {
            if agent.connect().is_ok() && agent.list_identities().is_ok() {
                for identity in agent.identities().unwrap_or_default() {
                    if agent.userauth(username, &identity).is_ok() {
                        return Ok(());
                    }
                }
            }
        }

        if !session.authenticated() {
            return Err(anyhow!(
                "Failed to authenticate user '{}'. No valid credentials provided.",
                username
            ));
        }

        Ok(())
    }
}

struct SftpWorker {
    _session: Session,
    sftp: Sftp,
    file_perm: Option<u32>,
    dir_perm: Option<u32>,
}

impl SftpWorker {
    fn stat(&self, remote_path: &str) -> Result<FileStat> {
        let p = Path::new(remote_path);
        let stat = self.sftp.stat(p).with_context(|| format!("Stat failed for {}", remote_path))?;
        let is_dir = stat.is_dir();
        let size = stat.size.unwrap_or(0);
        let mtime = stat.mtime.and_then(|m| Utc.timestamp_opt(m as i64, 0).single());
        let permissions = stat.perm;
        let name = p.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| remote_path.to_string());

        Ok(FileStat {
            name,
            path: remote_path.to_string(),
            is_dir,
            size,
            modified: mtime,
            permissions,
        })
    }

    fn read_dir(&self, remote_path: &str) -> Result<Vec<FileStat>> {
        let p = Path::new(remote_path);
        let entries = self.sftp.readdir(p).with_context(|| format!("readdir failed for {}", remote_path))?;

        let mut results = Vec::new();
        for (path_buf, stat) in entries {
            let name = path_buf
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();

            if name == "." || name == ".." {
                continue;
            }

            let is_dir = stat.is_dir();
            let size = stat.size.unwrap_or(0);
            let mtime = stat.mtime.and_then(|m| Utc.timestamp_opt(m as i64, 0).single());
            let permissions = stat.perm;
            let path_str = path_buf.to_string_lossy().to_string();

            results.push(FileStat {
                name,
                path: path_str,
                is_dir,
                size,
                modified: mtime,
                permissions,
            });
        }

        results.sort_by(|a, b| {
            if a.is_dir != b.is_dir {
                b.is_dir.cmp(&a.is_dir)
            } else {
                a.name.to_lowercase().cmp(&b.name.to_lowercase())
            }
        });

        Ok(results)
    }

    fn upload(&mut self, local_path: &Path, remote_path: &str, use_temp_file: bool) -> Result<u64> {
        let mut local_file = File::open(local_path)
            .with_context(|| format!("Failed to open local file: {}", local_path.display()))?;
        let metadata = local_file.metadata()?;
        let total_size = metadata.len();

        let parent_remote = Path::new(remote_path).parent();
        if let Some(parent) = parent_remote {
            let parent_str = parent.to_string_lossy();
            if !parent_str.is_empty() && parent_str != "/" {
                let _ = self.mkdir_all(&parent_str);
            }
        }

        let actual_remote_path = if use_temp_file {
            format!("{}.part", remote_path)
        } else {
            remote_path.to_string()
        };

        let mut remote_file = self
            .sftp
            .open_mode(
                Path::new(&actual_remote_path),
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
                self.file_perm.unwrap_or(0o644) as i32,
                OpenType::File,
            )
            .with_context(|| format!("Failed to open remote file: {}", actual_remote_path))?;

        let mut buffer = [0u8; 32 * 1024];
        let mut written_total: u64 = 0;

        loop {
            let bytes_read = local_file.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            remote_file.write_all(&buffer[..bytes_read])?;
            written_total += bytes_read as u64;
        }

        drop(remote_file);

        if use_temp_file {
            let src = Path::new(&actual_remote_path);
            let dst = Path::new(remote_path);
            let rename_res = self.sftp.rename(src, dst, Some(RenameFlags::OVERWRITE));
            if let Err(e) = rename_res {
                let _ = self.sftp.unlink(src);
                return Err(anyhow!("Failed to atomic rename temp file {}: {}", actual_remote_path, e));
            }
        }

        if let Some(perm) = self.file_perm {
            let _ = self.chmod(remote_path, perm);
        }

        Ok(written_total.max(total_size))
    }

    fn download(&mut self, remote_path: &str, local_path: &Path) -> Result<u64> {
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut remote_file = self
            .sftp
            .open(Path::new(remote_path))
            .with_context(|| format!("Failed to open remote file: {}", remote_path))?;

        let temp_local = local_path.with_extension("downloading.part");
        let mut local_file = File::create(&temp_local)?;

        let mut buffer = [0u8; 32 * 1024];
        let mut total_bytes: u64 = 0;

        loop {
            let bytes_read = remote_file.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            local_file.write_all(&buffer[..bytes_read])?;
            total_bytes += bytes_read as u64;
        }

        drop(local_file);
        std::fs::rename(&temp_local, local_path)?;
        Ok(total_bytes)
    }

    fn remove_file(&mut self, remote_path: &str) -> Result<()> {
        self.sftp
            .unlink(Path::new(remote_path))
            .with_context(|| format!("Failed to delete remote file: {}", remote_path))?;
        Ok(())
    }

    fn remove_dir(&mut self, remote_path: &str) -> Result<()> {
        self.sftp
            .rmdir(Path::new(remote_path))
            .with_context(|| format!("Failed to delete remote directory: {}", remote_path))?;
        Ok(())
    }

    fn mkdir_all(&mut self, remote_path: &str) -> Result<()> {
        let parts: Vec<&str> = remote_path.split('/').filter(|s| !s.is_empty()).collect();
        let mut current = String::new();
        if remote_path.starts_with('/') {
            current.push('/');
        }

        let dir_mode = self.dir_perm.unwrap_or(0o755) as i32;
        for part in parts {
            if !current.is_empty() && !current.ends_with('/') {
                current.push('/');
            }
            current.push_str(part);
            let p = Path::new(&current);
            if self.sftp.mkdir(p, dir_mode).is_ok() {
                if let Some(perm) = self.dir_perm {
                    let _ = self.chmod(&current, perm);
                }
            }
        }

        Ok(())
    }

    fn chmod(&self, remote_path: &str, mode: u32) -> Result<()> {
        let stat = ssh2::FileStat {
            size: None,
            uid: None,
            gid: None,
            perm: Some(mode),
            atime: None,
            mtime: None,
        };
        self.sftp
            .setstat(Path::new(remote_path), stat)
            .with_context(|| format!("Failed to chmod {:o} on {}", mode, remote_path))?;
        Ok(())
    }
}

#[async_trait]
impl RemoteFs for SftpBackend {
    async fn stat(&self, remote_path: &str) -> Result<FileStat> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(SftpCommand::Stat {
                path: remote_path.to_string(),
                reply,
            })
            .await
            .map_err(|_| anyhow!("SFTP worker disconnected"))?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .context("SFTP stat timed out")?
            .context("Failed to receive stat response")?
    }

    async fn read_dir(&self, remote_path: &str) -> Result<Vec<FileStat>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(SftpCommand::ReadDir {
                path: remote_path.to_string(),
                reply,
            })
            .await
            .map_err(|_| anyhow!("SFTP worker disconnected"))?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .context("SFTP read_dir timed out")?
            .context("Failed to receive read_dir response")?
    }

    async fn upload(&self, local_path: &Path, remote_path: &str, use_temp_file: bool) -> Result<u64> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(SftpCommand::Upload {
                local_path: local_path.to_path_buf(),
                remote_path: remote_path.to_string(),
                use_temp_file,
                reply,
            })
            .await
            .map_err(|_| anyhow!("SFTP worker disconnected"))?;
        rx.await.context("Failed to receive upload response")?
    }

    async fn download(&self, remote_path: &str, local_path: &Path) -> Result<u64> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(SftpCommand::Download {
                remote_path: remote_path.to_string(),
                local_path: local_path.to_path_buf(),
                reply,
            })
            .await
            .map_err(|_| anyhow!("SFTP worker disconnected"))?;
        rx.await.context("Failed to receive download response")?
    }

    async fn remove_file(&self, remote_path: &str) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(SftpCommand::RemoveFile {
                path: remote_path.to_string(),
                reply,
            })
            .await
            .map_err(|_| anyhow!("SFTP worker disconnected"))?;
        rx.await.context("Failed to receive remove_file response")?
    }

    async fn remove_dir(&self, remote_path: &str) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(SftpCommand::RemoveDir {
                path: remote_path.to_string(),
                reply,
            })
            .await
            .map_err(|_| anyhow!("SFTP worker disconnected"))?;
        rx.await.context("Failed to receive remove_dir response")?
    }

    async fn mkdir_all(&self, remote_path: &str) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(SftpCommand::MkdirAll {
                path: remote_path.to_string(),
                reply,
            })
            .await
            .map_err(|_| anyhow!("SFTP worker disconnected"))?;
        rx.await.context("Failed to receive mkdir_all response")?
    }

    async fn chmod(&self, remote_path: &str, mode: u32) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(SftpCommand::Chmod {
                path: remote_path.to_string(),
                mode,
                reply,
            })
            .await
            .map_err(|_| anyhow!("SFTP worker disconnected"))?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .context("SFTP chmod timed out")?
            .context("Failed to receive chmod response")?
    }

    async fn disconnect(&self) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(SftpCommand::Disconnect { reply }).await;
        let _ = rx.await;
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// FTP Implementation via AsyncFtpStream
// -----------------------------------------------------------------------------

pub struct FtpBackend {
    stream: Mutex<AsyncFtpStream>,
}

impl FtpBackend {
    pub async fn connect(config: SftpConfig) -> Result<Self> {
        let addr = format!("{}:{}", config.host, if config.port == 22 { 21 } else { config.port });
        let mut ftp = tokio::time::timeout(Duration::from_secs(15), AsyncFtpStream::connect(&addr))
            .await
            .with_context(|| format!("FTP connection timed out to {}", addr))?
            .with_context(|| format!("Failed to connect FTP to {}", addr))?;

        let password = config.password.as_deref().unwrap_or("");
        tokio::time::timeout(Duration::from_secs(15), ftp.login(config.username.as_str(), password))
            .await
            .with_context(|| format!("FTP login timed out for user {}", config.username))?
            .with_context(|| format!("FTP login failed for user {}", config.username))?;

        Ok(Self {
            stream: Mutex::new(ftp),
        })
    }
}

#[async_trait]
impl RemoteFs for FtpBackend {
    async fn stat(&self, remote_path: &str) -> Result<FileStat> {
        let mut ftp = self.stream.lock().await;
        let size = ftp.size(remote_path).await.unwrap_or(0) as u64;
        let name = Path::new(remote_path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| remote_path.to_string());

        Ok(FileStat {
            name,
            path: remote_path.to_string(),
            is_dir: false,
            size,
            modified: None,
            permissions: None,
        })
    }

    async fn read_dir(&self, remote_path: &str) -> Result<Vec<FileStat>> {
        let mut ftp = self.stream.lock().await;
        let list = ftp
            .nlst(Some(remote_path))
            .await
            .with_context(|| format!("Failed to list FTP dir {}", remote_path))?;

        let mut results = Vec::new();
        for item in list {
            let name = Path::new(&item)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| item.clone());
            if name == "." || name == ".." {
                continue;
            }
            results.push(FileStat {
                name,
                path: format!("{}/{}", remote_path.trim_end_matches('/'), item.trim_start_matches('/')),
                is_dir: false,
                size: 0,
                modified: None,
                permissions: None,
            });
        }
        Ok(results)
    }

    async fn upload(&self, local_path: &Path, remote_path: &str, _use_temp_file: bool) -> Result<u64> {
        let mut ftp = self.stream.lock().await;
        let mut local_file = tokio::fs::File::open(local_path).await?;
        let bytes = ftp.put_file(remote_path, &mut local_file).await?;
        Ok(bytes)
    }

    async fn download(&self, remote_path: &str, local_path: &Path) -> Result<u64> {
        let mut ftp = self.stream.lock().await;
        if let Some(parent) = local_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut stream = ftp.retr_as_stream(remote_path).await?;
        let mut local_file = tokio::fs::File::create(local_path).await?;
        let bytes = tokio::io::copy(&mut stream, &mut local_file).await?;
        stream.finish().await?;
        Ok(bytes)
    }

    async fn remove_file(&self, remote_path: &str) -> Result<()> {
        let mut ftp = self.stream.lock().await;
        ftp.rm(remote_path).await?;
        Ok(())
    }

    async fn remove_dir(&self, remote_path: &str) -> Result<()> {
        let mut ftp = self.stream.lock().await;
        ftp.rmdir(remote_path).await?;
        Ok(())
    }

    async fn mkdir_all(&self, remote_path: &str) -> Result<()> {
        let mut ftp = self.stream.lock().await;
        let parts: Vec<&str> = remote_path.split('/').filter(|s| !s.is_empty()).collect();
        let mut current = if remote_path.starts_with('/') {
            String::from("/")
        } else {
            String::new()
        };
        for part in parts {
            if !current.is_empty() && !current.ends_with('/') {
                current.push('/');
            }
            current.push_str(part);
            let _ = ftp.mkdir(&current).await;
        }
        Ok(())
    }

    async fn chmod(&self, remote_path: &str, mode: u32) -> Result<()> {
        let mut ftp = self.stream.lock().await;
        ftp.site(format!("CHMOD {:o} {}", mode, remote_path))
            .await
            .with_context(|| format!("FTP SITE CHMOD {:o} failed for {}", mode, remote_path))?;
        Ok(())
    }

    async fn disconnect(&self) -> Result<()> {
        let mut ftp = self.stream.lock().await;
        let _ = ftp.quit().await;
        Ok(())
    }
}

/// Factory function to construct RemoteFs client based on config
pub async fn create_remote_fs(config: SftpConfig) -> Result<Arc<dyn RemoteFs>> {
    match config.protocol.to_lowercase().as_str() {
        "sftp" => {
            let backend = SftpBackend::connect(config).await?;
            Ok(Arc::new(backend))
        }
        "ftp" => {
            let backend = FtpBackend::connect(config).await?;
            Ok(Arc::new(backend))
        }
        other => Err(anyhow!("Unsupported protocol: '{}'. Use 'sftp' or 'ftp'", other)),
    }
}
