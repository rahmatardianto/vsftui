use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use parking_lot::RwLock;
use tokio::sync::{Semaphore, mpsc};

use super::fs_provider::RemoteFs;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    Upload,
    Download,
    Delete,
    Mkdir,
}

impl TransferKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransferKind::Upload => "UPLOAD",
            TransferKind::Download => "DOWNLOAD",
            TransferKind::Delete => "DELETE",
            TransferKind::Mkdir => "MKDIR",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    Running,
    Success,
    Failed(String),
    Retrying(u32),
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct TransferJob {
    pub id: usize,
    pub kind: TransferKind,
    pub local_path: Option<PathBuf>,
    pub remote_path: String,
    pub use_temp_file: bool,
    pub status: JobStatus,
    pub size: u64,
    pub transferred: u64,
    pub speed_bytes_per_sec: f64,
    pub retry_count: u32,
    pub created_at: Instant,
    pub completed_at: Option<Instant>,
}

const MAX_COMPLETED_JOBS: usize = 500;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum ConveyerEvent {
    JobAdded(TransferJob),
    JobStarted(usize),
    JobCompleted { id: usize, result: Result<u64, String> },
    Log(String),
}

pub struct Conveyer {
    next_id: AtomicUsize,
    jobs: Arc<RwLock<Vec<TransferJob>>>,
    queue_tx: mpsc::UnboundedSender<TransferJob>,
    event_tx: mpsc::UnboundedSender<ConveyerEvent>,
}

impl Conveyer {
    pub fn new(
        remote_fs: Arc<dyn RemoteFs>,
        concurrency: usize,
        event_tx: mpsc::UnboundedSender<ConveyerEvent>,
    ) -> Self {
        let (queue_tx, mut queue_rx) = mpsc::unbounded_channel::<TransferJob>();
        let jobs = Arc::new(RwLock::new(Vec::<TransferJob>::new()));
        let semaphore = Arc::new(Semaphore::new(concurrency));

        let jobs_clone = Arc::clone(&jobs);
        let event_tx_clone = event_tx.clone();

        // Spawn background conveyer dispatcher loop
        tokio::spawn(async move {
            while let Some(job) = queue_rx.recv().await {
                let sem = Arc::clone(&semaphore);
                let fs = Arc::clone(&remote_fs);
                let jobs_ref = Arc::clone(&jobs_clone);
                let evt_tx = event_tx_clone.clone();

                tokio::spawn(async move {
                    let _permit = match sem.acquire().await {
                        Ok(p) => p,
                        Err(_) => return,
                    };

                    // Update job status to Running
                    {
                        let mut list = jobs_ref.write();
                        if let Some(j) = list.iter_mut().find(|item| item.id == job.id) {
                            j.status = JobStatus::Running;
                        }
                    }
                    let _ = evt_tx.send(ConveyerEvent::JobStarted(job.id));

                    let max_retries = 3;
                    let mut current_retry = 0;
                    let mut success = false;
                    let mut total_transferred = 0u64;
                    let mut last_error = String::new();
                    let start_time = Instant::now();

                    while current_retry <= max_retries && !success {
                        let res: Result<u64> = match job.kind {
                            TransferKind::Upload => {
                                if let Some(ref lpath) = job.local_path {
                                    fs.upload(lpath, &job.remote_path, job.use_temp_file).await
                                } else {
                                    Err(anyhow::anyhow!("Local path missing for upload"))
                                }
                            }
                            TransferKind::Download => {
                                if let Some(ref lpath) = job.local_path {
                                    fs.download(&job.remote_path, lpath).await
                                } else {
                                    Err(anyhow::anyhow!("Local path missing for download"))
                                }
                            }
                            TransferKind::Delete => {
                                fs.remove_file(&job.remote_path).await.map(|_| 0)
                            }
                            TransferKind::Mkdir => {
                                fs.mkdir_all(&job.remote_path).await.map(|_| 0)
                            }
                        };

                        match res {
                            Ok(bytes) => {
                                total_transferred = bytes;
                                success = true;
                            }
                            Err(e) => {
                                last_error = e.to_string();
                                current_retry += 1;
                                if current_retry <= max_retries {
                                    {
                                        let mut list = jobs_ref.write();
                                        if let Some(j) = list.iter_mut().find(|item| item.id == job.id) {
                                            j.status = JobStatus::Retrying(current_retry);
                                            j.retry_count = current_retry;
                                        }
                                    }
                                    let _ = evt_tx.send(ConveyerEvent::Log(format!(
                                        "Job #{} ({}) retry {}/{}: {}",
                                        job.id, job.remote_path, current_retry, max_retries, last_error
                                    )));
                                    tokio::time::sleep(tokio::time::Duration::from_millis(500 * (1 << current_retry))).await;
                                }
                            }
                        }
                    }

                    let elapsed = start_time.elapsed().as_secs_f64();
                    let speed = if elapsed > 0.0 {
                        total_transferred as f64 / elapsed
                    } else {
                        total_transferred as f64
                    };

                    {
                        let mut list = jobs_ref.write();
                        if let Some(j) = list.iter_mut().find(|item| item.id == job.id) {
                            j.transferred = total_transferred;
                            j.speed_bytes_per_sec = speed;
                            j.completed_at = Some(Instant::now());
                            if success {
                                j.status = JobStatus::Success;
                            } else {
                                j.status = JobStatus::Failed(last_error.clone());
                            }
                        }
                    }

                    let outcome = if success {
                        Ok(total_transferred)
                    } else {
                        Err(last_error)
                    };

                    let _ = evt_tx.send(ConveyerEvent::JobCompleted {
                        id: job.id,
                        result: outcome,
                    });
                });
            }
        });

        Self {
            next_id: AtomicUsize::new(1),
            jobs,
            queue_tx,
            event_tx,
        }
    }

    pub fn enqueue(
        &self,
        kind: TransferKind,
        local_path: Option<PathBuf>,
        remote_path: String,
        use_temp_file: bool,
    ) -> usize {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let size = if let Some(ref p) = local_path {
            std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        let job = TransferJob {
            id,
            kind,
            local_path,
            remote_path,
            use_temp_file,
            status: JobStatus::Pending,
            size,
            transferred: 0,
            speed_bytes_per_sec: 0.0,
            retry_count: 0,
            created_at: Instant::now(),
            completed_at: None,
        };

        {
            let mut list = self.jobs.write();
            let completed_count = list
                .iter()
                .filter(|j| matches!(j.status, JobStatus::Success | JobStatus::Failed(_)))
                .count();
            if completed_count >= MAX_COMPLETED_JOBS {
                let mut to_drop = completed_count - MAX_COMPLETED_JOBS + 1;
                list.retain(|j| {
                    if to_drop > 0 && matches!(j.status, JobStatus::Success | JobStatus::Failed(_)) {
                        to_drop -= 1;
                        false
                    } else {
                        true
                    }
                });
            }
            list.push(job.clone());
        }

        let _ = self.event_tx.send(ConveyerEvent::JobAdded(job.clone()));
        let _ = self.queue_tx.send(job);
        id
    }

    pub fn get_jobs(&self) -> Vec<TransferJob> {
        self.jobs.read().clone()
    }

    pub fn clear_completed(&self) {
        let mut list = self.jobs.write();
        list.retain(|j| matches!(j.status, JobStatus::Pending | JobStatus::Running | JobStatus::Retrying(_)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::AtomicU32;
    use std::time::Duration;
    use async_trait::async_trait;
    use crate::transfer::fs_provider::FileStat;

    struct FlakyRemoteFs {
        attempts: AtomicU32,
        fail_times: u32,
    }

    #[async_trait]
    impl RemoteFs for FlakyRemoteFs {
        async fn stat(&self, _: &str) -> Result<FileStat> {
            anyhow::bail!("unused")
        }
        async fn read_dir(&self, _: &str) -> Result<Vec<FileStat>> {
            Ok(vec![])
        }
        async fn upload(&self, _: &Path, _: &str, _: bool) -> Result<u64> {
            let prev = self.attempts.fetch_add(1, Ordering::SeqCst);
            if prev < self.fail_times {
                anyhow::bail!("transient failure {}", prev + 1);
            }
            Ok(42)
        }
        async fn download(&self, _: &str, _: &Path) -> Result<u64> {
            Ok(0)
        }
        async fn remove_file(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn remove_dir(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn mkdir_all(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn chmod(&self, _: &str, _: u32) -> Result<()> {
            Ok(())
        }
        async fn disconnect(&self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_conveyer_retries_and_clears_completed() {
        let fs = Arc::new(FlakyRemoteFs {
            attempts: AtomicU32::new(0),
            fail_times: 1,
        });
        let (tx, mut rx) = mpsc::unbounded_channel();
        let conveyer = Conveyer::new(fs, 2, tx);

        let id = conveyer.enqueue(
            TransferKind::Upload,
            Some(PathBuf::from("Cargo.toml")),
            "/remote/Cargo.toml".to_string(),
            false,
        );

        let mut completed_ok = false;
        while let Some(evt) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("conveyer timed out")
        {
            if let ConveyerEvent::JobCompleted { id: eid, result } = evt {
                assert_eq!(eid, id);
                assert_eq!(result.unwrap(), 42);
                completed_ok = true;
                break;
            }
        }
        assert!(completed_ok);

        let jobs = conveyer.get_jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, JobStatus::Success);
        assert_eq!(jobs[0].retry_count, 1);

        conveyer.clear_completed();
        assert!(conveyer.get_jobs().is_empty());
    }
}
