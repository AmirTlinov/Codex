//! Adapter between codebase-context and core's CodebaseSearchProvider trait

use crate::context_manager::CodebaseContext;
use crate::context_manager::CodebaseSearchProvider;
use codex_codebase_context::ContextProvider;
use codex_codebase_context::ContextSearchMetadata;
use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Adapter that wraps ContextProvider to implement CodebaseSearchProvider
pub(crate) struct CodebaseContextAdapter {
    provider: Arc<Mutex<ContextProvider>>,
}

impl std::fmt::Debug for CodebaseContextAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodebaseContextAdapter")
            .field("provider", &"Arc<Mutex<ContextProvider>>")
            .finish()
    }
}

impl CodebaseContextAdapter {
    /// Create new adapter
    pub(crate) fn new(provider: Arc<Mutex<ContextProvider>>) -> Self {
        Self { provider }
    }
}

impl CodebaseSearchProvider for CodebaseContextAdapter {
    fn provide_context<'a>(
        &'a mut self,
        query: &'a str,
        token_budget: usize,
        metadata: Option<&'a ContextSearchMetadata>,
    ) -> BoxFuture<'a, anyhow::Result<Option<CodebaseContext>>> {
        Box::pin(async move {
            let result = self
                .provider
                .lock()
                .await
                .provide_context_with_metadata(query, token_budget, metadata)
                .await?;

            Ok(result.map(|ctx| CodebaseContext {
                formatted_context: ctx.formatted_context,
                chunks_count: ctx.chunks.len(),
                tokens_used: ctx.tokens_used,
            }))
        })
    }
}
