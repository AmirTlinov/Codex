//! Session-wide mutable state.

use codex_protocol::approvals::SandboxCommandAssessment;
use codex_protocol::approvals::SandboxRiskHistoryEntry;
use codex_protocol::models::ResponseItem;

use crate::conversation_history::ConversationHistory;
use crate::protocol::RateLimitSnapshot;
use crate::protocol::TokenUsage;
use crate::protocol::TokenUsageInfo;
use std::collections::VecDeque;

/// Persistent, session-scoped state previously stored directly on `Session`.
#[derive(Default)]
pub(crate) struct SessionState {
    pub(crate) history: ConversationHistory,
    pub(crate) token_info: Option<TokenUsageInfo>,
    pub(crate) latest_rate_limits: Option<RateLimitSnapshot>,
    pub(crate) recent_risk_assessments: VecDeque<RiskAssessmentEntry>,
}

impl SessionState {
    /// Create a new session state mirroring previous `State::default()` semantics.
    pub(crate) fn new() -> Self {
        Self {
            history: ConversationHistory::new(),
            ..Default::default()
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

    pub(crate) fn history_snapshot(&self) -> Vec<ResponseItem> {
        self.history.contents()
    }

    pub(crate) fn replace_history(&mut self, items: Vec<ResponseItem>) {
        self.history.replace(items);
    }

    // Token/rate limit helpers
    pub(crate) fn update_token_info_from_usage(
        &mut self,
        usage: &TokenUsage,
        model_context_window: Option<u64>,
    ) {
        self.token_info = TokenUsageInfo::new_or_append(
            &self.token_info,
            &Some(usage.clone()),
            model_context_window,
        );
    }

    pub(crate) fn set_rate_limits(&mut self, snapshot: RateLimitSnapshot) {
        self.latest_rate_limits = Some(snapshot);
    }

    pub(crate) fn token_info_and_rate_limits(
        &self,
    ) -> (Option<TokenUsageInfo>, Option<RateLimitSnapshot>) {
        (self.token_info.clone(), self.latest_rate_limits.clone())
    }

    pub(crate) fn set_token_usage_full(&mut self, context_window: u64) {
        match &mut self.token_info {
            Some(info) => info.fill_to_context_window(context_window),
            None => {
                self.token_info = Some(TokenUsageInfo::full_context_window(context_window));
            }
        }
    }

    pub(crate) fn record_risk_assessment(
        &mut self,
        call_id: String,
        assessment: SandboxCommandAssessment,
    ) {
        const MAX_RECENT_RISK_ASSESSMENTS: usize = 20;
        if let Some(last) = self
            .recent_risk_assessments
            .back_mut()
            .filter(|last| last.call_id == call_id)
        {
            if last.assessment != assessment {
                last.assessment = assessment;
            }
            return;
        }
        if self.recent_risk_assessments.len() >= MAX_RECENT_RISK_ASSESSMENTS {
            self.recent_risk_assessments.pop_front();
        }
        self.recent_risk_assessments.push_back(RiskAssessmentEntry {
            call_id,
            assessment,
        });
    }

    pub(crate) fn risk_assessment_history(&self) -> Vec<RiskAssessmentEntry> {
        self.recent_risk_assessments.iter().cloned().collect()
    }

    // Pending input/approval moved to TurnState.
}

#[derive(Clone)]
pub(crate) struct RiskAssessmentEntry {
    pub call_id: String,
    pub assessment: SandboxCommandAssessment,
}

impl From<RiskAssessmentEntry> for SandboxRiskHistoryEntry {
    fn from(value: RiskAssessmentEntry) -> Self {
        Self {
            call_id: value.call_id,
            assessment: value.assessment,
        }
    }
}
