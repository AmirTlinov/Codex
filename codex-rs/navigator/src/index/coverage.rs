use crate::proto::CoverageDiagnostics;
use crate::proto::CoverageGap;
use crate::proto::CoverageReason;
use std::collections::VecDeque;
use tokio::sync::Mutex;

const DEFAULT_LIMIT: usize = 32;

pub(crate) struct CoverageTracker {
    inner: Mutex<CoverageState>,
}

struct CoverageState {
    pending: VecDeque<CoverageGap>,
    skipped: VecDeque<CoverageGap>,
    errors: VecDeque<CoverageGap>,
    limit: usize,
}

impl CoverageTracker {
    pub fn new(limit: Option<usize>) -> Self {
        let cap = limit.unwrap_or(DEFAULT_LIMIT).max(1);
        Self {
            inner: Mutex::new(CoverageState {
                pending: VecDeque::with_capacity(cap),
                skipped: VecDeque::with_capacity(cap),
                errors: VecDeque::with_capacity(cap),
                limit: cap,
            }),
        }
    }

    pub async fn record_pending(&self, path: impl ToString) {
        let mut guard = self.inner.lock().await;
        let gap = CoverageGap {
            path: path.to_string(),
            reason: CoverageReason::PendingIngest,
        };
        guard.upsert_pending(gap);
    }

    pub async fn record_indexed(&self, path: &str) {
        let mut guard = self.inner.lock().await;
        guard.remove(path);
    }

    pub async fn record_skipped(&self, path: impl ToString, reason: CoverageReason) {
        let mut guard = self.inner.lock().await;
        guard.remove(path.to_string().as_str());
        let gap = CoverageGap {
            path: path.to_string(),
            reason,
        };
        guard.upsert_skipped(gap);
    }

    pub async fn record_error(&self, path: impl ToString, reason: CoverageReason) {
        let mut guard = self.inner.lock().await;
        let gap = CoverageGap {
            path: path.to_string(),
            reason,
        };
        guard.upsert_errors(gap);
    }

    pub async fn replace_skipped(&self, gaps: Vec<CoverageGap>) {
        let mut guard = self.inner.lock().await;
        guard.skipped.clear();
        for gap in gaps {
            guard.upsert_skipped(gap);
        }
    }

    pub async fn clear_pending(&self) {
        let mut guard = self.inner.lock().await;
        guard.pending.clear();
    }

    pub async fn diagnostics(&self) -> CoverageDiagnostics {
        let guard = self.inner.lock().await;
        CoverageDiagnostics {
            pending: guard.pending.iter().cloned().collect(),
            skipped: guard.skipped.iter().cloned().collect(),
            errors: guard.errors.iter().cloned().collect(),
        }
    }
}

impl CoverageState {
    fn remove(&mut self, path: &str) {
        Self::remove_from(&mut self.pending, path);
        Self::remove_from(&mut self.skipped, path);
        Self::remove_from(&mut self.errors, path);
    }

    fn remove_from(bucket: &mut VecDeque<CoverageGap>, path: &str) {
        if let Some(pos) = bucket.iter().position(|gap| gap.path == path) {
            bucket.remove(pos);
        }
    }

    fn upsert_pending(&mut self, gap: CoverageGap) {
        Self::upsert_bucket(&mut self.pending, self.limit, gap);
    }

    fn upsert_skipped(&mut self, gap: CoverageGap) {
        Self::upsert_bucket(&mut self.skipped, self.limit, gap);
    }

    fn upsert_errors(&mut self, gap: CoverageGap) {
        Self::upsert_bucket(&mut self.errors, self.limit, gap);
    }

    fn upsert_bucket(bucket: &mut VecDeque<CoverageGap>, limit: usize, gap: CoverageGap) {
        Self::remove_from(bucket, &gap.path);
        bucket.push_back(gap);
        while bucket.len() > limit {
            bucket.pop_front();
        }
    }
}
