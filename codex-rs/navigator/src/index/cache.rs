use crate::proto::QueryId;
use crate::proto::SearchFilters;
use anyhow::Result;
use lru::LruCache;
use serde::Deserialize;
use serde::Serialize;
use std::fs;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Mutex;

const CACHE_CAPACITY: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedQuery {
    pub candidate_ids: Vec<String>,
    pub query: Option<String>,
    pub filters: SearchFilters,
    #[serde(default)]
    pub parent: Option<QueryId>,
}

pub struct QueryCache {
    dir: PathBuf,
    memory: Mutex<LruCache<String, CachedQuery>>,
}

impl QueryCache {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            memory: Mutex::new(LruCache::new(
                NonZeroUsize::new(CACHE_CAPACITY)
                    .unwrap_or_else(|| unreachable!("cache capacity must be non-zero")),
            )),
        }
    }

    pub fn store(&self, id: QueryId, entry: CachedQuery) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let key = id.to_string();
        self.lock().put(key.clone(), entry.clone());
        let path = self.dir.join(format!("{key}.json"));
        let data = serde_json::to_vec_pretty(&entry)?;
        fs::write(path, data)?;
        Ok(())
    }

    pub fn load(&self, id: QueryId) -> Result<Option<CachedQuery>> {
        let key = id.to_string();
        if let Some(value) = self.lock().get(&key).cloned() {
            return Ok(Some(value));
        }
        let path = self.dir.join(format!("{key}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read(path)?;
        let entry: CachedQuery = serde_json::from_slice(&data)?;
        self.lock().put(key, entry.clone());
        Ok(Some(entry))
    }
}

impl QueryCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, LruCache<String, CachedQuery>> {
        match self.memory.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}
