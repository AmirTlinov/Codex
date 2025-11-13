use crate::index::model::FileFingerprint;
use crate::index::model::FileText;
use crate::index::model::IndexSnapshot;
use anyhow::Context;
use async_channel::Receiver;
use async_channel::Sender;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::spawn_blocking;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::warn;

const TEXT_QUEUE_BOUND: usize = 96;

#[derive(Clone)]
pub(crate) struct TextIngestSender {
    tx: Sender<PendingText>,
}

pub(crate) struct TextIngestor {
    tx: Sender<PendingText>,
}

impl TextIngestor {
    pub(crate) fn new(snapshot: Arc<RwLock<IndexSnapshot>>, shutdown: CancellationToken) -> Self {
        let (tx, rx) = async_channel::bounded(TEXT_QUEUE_BOUND);
        let workers = worker_count();
        for _ in 0..workers {
            spawn_worker(rx.clone(), snapshot.clone(), shutdown.clone());
        }
        Self { tx }
    }

    pub(crate) fn sender(&self) -> TextIngestSender {
        TextIngestSender {
            tx: self.tx.clone(),
        }
    }
}

impl TextIngestSender {
    pub(crate) fn send_blocking(
        &self,
        payload: PendingText,
    ) -> Result<(), async_channel::SendError<PendingText>> {
        self.tx.send_blocking(payload)
    }
}

#[derive(Clone)]
pub(crate) struct PendingText {
    pub path: String,
    pub fingerprint: FileFingerprint,
    pub bytes: Arc<[u8]>,
}

impl PendingText {
    pub(crate) fn new(path: String, fingerprint: FileFingerprint, bytes: Arc<[u8]>) -> Self {
        Self {
            path,
            fingerprint,
            bytes,
        }
    }
}

fn worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|value| value.get().clamp(2, 8))
        .unwrap_or(4)
}

fn spawn_worker(
    rx: Receiver<PendingText>,
    snapshot: Arc<RwLock<IndexSnapshot>>,
    shutdown: CancellationToken,
) {
    tokio::spawn(async move {
        loop {
            let job = tokio::select! {
                _ = shutdown.cancelled() => break,
                result = rx.recv() => match result {
                    Ok(job) => job,
                    Err(_) => break,
                },
            };
            if let Err(err) = process_job(job, snapshot.clone()).await {
                warn!("navigator text ingest failed: {err:?}");
            }
        }
    });
}

async fn process_job(job: PendingText, snapshot: Arc<RwLock<IndexSnapshot>>) -> anyhow::Result<()> {
    let path = job.path.clone();
    let fingerprint = job.fingerprint.clone();
    let bytes = job.bytes.clone();
    let text = spawn_blocking(move || {
        let content = std::str::from_utf8(&bytes).context("text payload not utf-8")?;
        FileText::from_content(content)
    })
    .await
    .context("text ingest worker join")??;
    let mut guard = snapshot.write().await;
    if guard
        .files
        .get(&path)
        .map(|entry| entry.fingerprint == fingerprint)
        .unwrap_or(false)
    {
        guard.text.insert(path, text);
    } else {
        debug!("skipping stale text ingest for {path} (fingerprint changed while job was pending)");
    }
    Ok(())
}
