//! Session-wide mutable state.

use codex_protocol::models::ResponseItem;

use crate::codex::SessionConfiguration;
use crate::codex::TurnContext;
use crate::context_manager::CodebaseSearchProvider;
use crate::context_manager::ContextManager;
use crate::protocol::RateLimitSnapshot;
use crate::protocol::TokenUsage;
use crate::protocol::TokenUsageInfo;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Persistent, session-scoped state previously stored directly on `Session`.
pub(crate) struct SessionState {
    pub(crate) session_configuration: SessionConfiguration,
    pub(crate) history: ContextManager,
    pub(crate) latest_rate_limits: Option<RateLimitSnapshot>,
}

impl SessionState {
    /// Create a new session state mirroring previous `State::default()` semantics.
    pub(crate) fn new(session_configuration: SessionConfiguration) -> Self {
        Self {
            session_configuration,
            history: ContextManager::new(),
            latest_rate_limits: None,
        }
    }

    /// Create session state with codebase search provider
    pub(crate) fn new_with_codebase(
        session_configuration: SessionConfiguration,
        codebase_provider: Option<Arc<Mutex<Box<dyn CodebaseSearchProvider>>>>,
        codebase_config: crate::config::types::CodebaseSearchConfig,
    ) -> Self {
        Self {
            session_configuration,
            history: ContextManager::new_with_config(codebase_provider, codebase_config),
            latest_rate_limits: None,
        }
    }

    // History helpers
    pub(crate) fn record_items<I>(&mut self, items: I)
    where
        I: IntoIterator,
        I::Item: std::ops::Deref<Target = ResponseItem>,
    {
        self.history.record_items(items)
    }

    /// Record items with automatic codebase context injection. When
    /// `capture_recorded_items` is true, returns the exact sequence (including
    /// injected context) that was appended to history.
    pub(crate) async fn record_items_with_context<I>(
        &mut self,
        turn_context: &TurnContext,
        items: I,
        capture_recorded_items: bool,
    ) -> anyhow::Result<Option<Vec<ResponseItem>>>
    where
        I: IntoIterator,
        I::Item: std::ops::Deref<Target = ResponseItem>,
    {
        self.history
            .record_items_with_context(items, capture_recorded_items, Some(&turn_context.cwd))
            .await
    }

    pub(crate) fn clone_history(&self) -> ContextManager {
        self.history.clone()
    }

    pub(crate) fn replace_history(&mut self, items: Vec<ResponseItem>) {
        self.history.replace(items);
    }

    pub(crate) fn set_token_info(&mut self, info: Option<TokenUsageInfo>) {
        self.history.set_token_info(info);
    }

    // Token/rate limit helpers
    pub(crate) fn update_token_info_from_usage(
        &mut self,
        usage: &TokenUsage,
        model_context_window: Option<i64>,
    ) {
        self.history.update_token_info(usage, model_context_window);
    }

    pub(crate) fn token_info(&self) -> Option<TokenUsageInfo> {
        self.history.token_info()
    }

    pub(crate) fn set_codebase_provider(
        &mut self,
        provider: Arc<Mutex<Box<dyn CodebaseSearchProvider>>>,
    ) {
        self.history.set_codebase_provider(provider);
    }

    pub(crate) fn set_rate_limits(&mut self, snapshot: RateLimitSnapshot) {
        self.latest_rate_limits = Some(snapshot);
    }

    pub(crate) fn token_info_and_rate_limits(
        &self,
    ) -> (Option<TokenUsageInfo>, Option<RateLimitSnapshot>) {
        (self.token_info(), self.latest_rate_limits.clone())
    }

    pub(crate) fn set_token_usage_full(&mut self, context_window: i64) {
        self.history.set_token_usage_full(context_window);
    }
}
