use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use crate::AuthManager;
use crate::ModelProviderInfo;
use crate::client::ModelClient;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::config::Config;
use crate::protocol::SandboxPolicy;
use askama::Template;
use codex_otel::otel_event_manager::OtelEventManager;
use codex_protocol::ConversationId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SandboxCommandAssessment;
use futures::StreamExt;
use lru::LruCache;
#[cfg(test)]
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tracing::warn;

const SANDBOX_ASSESSMENT_TIMEOUT: Duration = Duration::from_secs(5);

const SANDBOX_RISK_CATEGORY_VALUES: &[&str] = &[
    "data_deletion",
    "data_exfiltration",
    "privilege_escalation",
    "system_modification",
    "network_access",
    "resource_exhaustion",
    "compliance",
];

#[derive(Template)]
#[template(path = "sandboxing/assessment_prompt.md", escape = "none")]
struct SandboxAssessmentPromptTemplate<'a> {
    platform: &'a str,
    sandbox_policy: &'a str,
    filesystem_roots: Option<&'a str>,
    working_directory: &'a str,
    command_argv: &'a str,
    command_joined: &'a str,
    sandbox_failure_message: Option<&'a str>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn assess_command(
    config: Arc<Config>,
    provider: ModelProviderInfo,
    auth_manager: Arc<AuthManager>,
    parent_otel: &OtelEventManager,
    conversation_id: ConversationId,
    _call_id: &str,
    command: &[String],
    sandbox_policy: &SandboxPolicy,
    cwd: &Path,
    failure_message: Option<&str>,
) -> Option<SandboxCommandAssessment> {
    if !config.experimental_sandbox_command_assessment || command.is_empty() {
        return None;
    }

    let command_json = serde_json::to_string(command).unwrap_or_else(|_| "[]".to_string());
    let command_joined =
        shlex::try_join(command.iter().map(String::as_str)).unwrap_or_else(|_| command.join(" "));
    let failure = failure_message
        .map(str::trim)
        .filter(|msg| !msg.is_empty())
        .map(str::to_string);

    let cwd_str = cwd.to_string_lossy().to_string();
    let sandbox_summary = summarize_sandbox_policy(sandbox_policy);
    let mut roots = sandbox_roots_for_prompt(sandbox_policy, cwd);
    roots.sort();
    roots.dedup();

    let platform = std::env::consts::OS;
    let roots_formatted = roots.iter().map(|root| root.to_string_lossy().to_string());
    let filesystem_roots = match roots_formatted.collect::<Vec<_>>() {
        collected if collected.is_empty() => None,
        collected => Some(collected.join(", ")),
    };

    let prompt_template = SandboxAssessmentPromptTemplate {
        platform,
        sandbox_policy: sandbox_summary.as_str(),
        filesystem_roots: filesystem_roots.as_deref(),
        working_directory: cwd_str.as_str(),
        command_argv: command_json.as_str(),
        command_joined: command_joined.as_str(),
        sandbox_failure_message: failure.as_deref(),
    };
    let rendered_prompt = match prompt_template.render() {
        Ok(rendered) => rendered,
        Err(err) => {
            warn!("failed to render sandbox assessment prompt: {err}");
            return None;
        }
    };
    let (system_prompt_section, user_prompt_section) = match rendered_prompt.split_once("\n---\n") {
        Some(split) => split,
        None => {
            warn!("rendered sandbox assessment prompt missing separator");
            return None;
        }
    };
    let system_prompt = system_prompt_section
        .strip_prefix("System Prompt:\n")
        .unwrap_or(system_prompt_section)
        .trim()
        .to_string();
    let user_prompt = user_prompt_section
        .strip_prefix("User Prompt:\n")
        .unwrap_or(user_prompt_section)
        .trim()
        .to_string();

    let prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text: user_prompt }],
        }],
        tools: Vec::new(),
        parallel_tool_calls: false,
        base_instructions_override: Some(system_prompt),
        output_schema: Some(sandbox_assessment_schema()),
    };

    let child_otel =
        parent_otel.with_model(config.model.as_str(), config.model_family.slug.as_str());

    let client = ModelClient::new(
        Arc::clone(&config),
        Some(auth_manager),
        child_otel,
        provider,
        config.model_reasoning_effort,
        config.model_reasoning_summary,
        conversation_id,
    );

    let start = Instant::now();
    let assessment_result = timeout(SANDBOX_ASSESSMENT_TIMEOUT, async move {
        let mut stream = client.stream(&prompt).await?;
        let mut last_json: Option<String> = None;
        while let Some(event) = stream.next().await {
            match event {
                Ok(ResponseEvent::OutputItemDone(item)) => {
                    if let Some(text) = response_item_text(&item) {
                        last_json = Some(text);
                    }
                }
                Ok(ResponseEvent::RateLimits(_)) => {}
                Ok(ResponseEvent::Completed { .. }) => break,
                Ok(_) => continue,
                Err(err) => return Err(err),
            }
        }
        Ok(last_json)
    })
    .await;
    let _duration = start.elapsed();
    match assessment_result {
        Ok(Ok(Some(raw))) => match serde_json::from_str::<SandboxCommandAssessment>(raw.trim()) {
            Ok(assessment) => {
                return Some(assessment);
            }
            Err(err) => {
                warn!("failed to parse sandbox assessment JSON: {err}");
            }
        },
        Ok(Ok(None)) => {
            warn!("sandbox assessment response did not include any message");
        }
        Ok(Err(err)) => {
            warn!("sandbox assessment failed: {err}");
        }
        Err(_) => {
            warn!("sandbox assessment timed out");
        }
    }

    None
}

fn summarize_sandbox_policy(policy: &SandboxPolicy) -> String {
    match policy {
        SandboxPolicy::DangerFullAccess => "danger-full-access".to_string(),
        SandboxPolicy::ReadOnly => "read-only".to_string(),
        SandboxPolicy::WorkspaceWrite { network_access, .. } => {
            let network = if *network_access {
                "network"
            } else {
                "no-network"
            };
            format!("workspace-write (network_access={network})")
        }
    }
}

fn sandbox_roots_for_prompt(policy: &SandboxPolicy, cwd: &Path) -> Vec<PathBuf> {
    let mut roots = vec![cwd.to_path_buf()];
    if let SandboxPolicy::WorkspaceWrite { writable_roots, .. } = policy {
        roots.extend(writable_roots.iter().cloned());
    }
    roots
}

fn sandbox_assessment_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["description", "risk_level", "risk_categories"],
        "properties": {
            "description": {
                "type": "string",
                "minLength": 1,
                "maxLength": 500
            },
            "risk_level": {
                "type": "string",
                "enum": ["low", "medium", "high"]
            },
            "risk_categories": {
                "type": "array",
                "items": {
                    "type": "string",
                    "enum": SANDBOX_RISK_CATEGORY_VALUES
                }
            }
        },
        "additionalProperties": false
    })
}

fn response_item_text(item: &ResponseItem) -> Option<String> {
    match item {
        ResponseItem::Message { content, .. } => {
            let mut buffers: Vec<&str> = Vec::new();
            for segment in content {
                match segment {
                    ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                        if !text.is_empty() {
                            buffers.push(text);
                        }
                    }
                    ContentItem::InputImage { .. } => {}
                }
            }
            if buffers.is_empty() {
                None
            } else {
                Some(buffers.join("\n"))
            }
        }
        ResponseItem::FunctionCallOutput { output, .. } => Some(output.content.clone()),
        _ => None,
    }
}

const DEFAULT_CACHE_CAPACITY: usize = 64;
const SANDBOX_ASSESSMENT_CACHE_TTL: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
pub struct SandboxAssessmentRequest {
    pub config: Arc<Config>,
    pub provider: ModelProviderInfo,
    pub auth_manager: Option<Arc<AuthManager>>,
    pub otel_event_manager: OtelEventManager,
    pub conversation_id: ConversationId,
    pub call_id: String,
    pub command: Vec<String>,
    pub sandbox_policy: SandboxPolicy,
    pub cwd: PathBuf,
    pub failure_message: Option<String>,
}

impl SandboxAssessmentRequest {
    fn cache_key(&self) -> String {
        format!(
            "{:?}|{}|{:?}",
            self.sandbox_policy,
            self.cwd.display(),
            self.command
        )
    }
}

struct CachedAssessment {
    assessment: SandboxCommandAssessment,
    expires_at: Instant,
}

#[derive(Clone)]
pub struct SandboxAssessmentService {
    inner: Arc<SandboxAssessmentInner>,
}

struct SandboxAssessmentInner {
    cache: Mutex<LruCache<String, CachedAssessment>>,
    pending: Mutex<HashMap<String, Vec<oneshot::Sender<Option<SandboxCommandAssessment>>>>>,
}

impl SandboxAssessmentService {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        // SAFETY: capacity is clamped to at least 1 above, so constructing NonZeroUsize is safe.
        let cache_capacity = unsafe { NonZeroUsize::new_unchecked(capacity) };
        let inner = Arc::new(SandboxAssessmentInner {
            cache: Mutex::new(LruCache::new(cache_capacity)),
            pending: Mutex::new(HashMap::new()),
        });

        Self { inner }
    }

    pub async fn assess(
        &self,
        request: SandboxAssessmentRequest,
    ) -> Option<SandboxCommandAssessment> {
        if request.command.is_empty() || !request.config.experimental_sandbox_command_assessment {
            return None;
        }

        let key = request.cache_key();
        if let Some(hit) = self.cached(&key) {
            return Some(hit);
        }

        if let Some(rx) = self.enqueue_request(&key) {
            return rx.await.unwrap_or(None);
        }

        let result = perform_assessment(request).await;
        if let Some(assessment) = &result {
            let mut cache = self.inner.cache.lock();
            cache.put(
                key.clone(),
                CachedAssessment {
                    assessment: assessment.clone(),
                    expires_at: Instant::now() + SANDBOX_ASSESSMENT_CACHE_TTL,
                },
            );
        }

        self.finish_pending(key, result.clone());
        result
    }

    fn cached(&self, key: &str) -> Option<SandboxCommandAssessment> {
        let mut cache = self.inner.cache.lock();
        match cache.get(key) {
            Some(entry) if Instant::now() <= entry.expires_at => {
                return Some(entry.assessment.clone());
            }
            _ => {}
        }
        None
    }

    fn enqueue_request(
        &self,
        key: &str,
    ) -> Option<oneshot::Receiver<Option<SandboxCommandAssessment>>> {
        let mut pending = self.inner.pending.lock();
        if let Some(waiters) = pending.get_mut(key) {
            let (tx, rx) = oneshot::channel();
            waiters.push(tx);
            Some(rx)
        } else {
            pending.insert(key.to_string(), Vec::new());
            None
        }
    }

    fn finish_pending(&self, key: String, result: Option<SandboxCommandAssessment>) {
        let waiters = {
            let mut pending = self.inner.pending.lock();
            pending.remove(&key)
        };
        if let Some(waiters) = waiters {
            for waiter in waiters {
                let _ = waiter.send(result.clone());
            }
        }
    }

    #[cfg(test)]
    pub fn set_test_override(&self, assessment: SandboxCommandAssessment) {
        *TEST_OVERRIDE.lock() = Some(assessment);
    }
}

impl Default for SandboxAssessmentService {
    fn default() -> Self {
        Self::new(DEFAULT_CACHE_CAPACITY)
    }
}

async fn perform_assessment(request: SandboxAssessmentRequest) -> Option<SandboxCommandAssessment> {
    #[cfg(test)]
    {
        if let Some(override_value) = TEST_OVERRIDE.lock().take() {
            return Some(override_value);
        }
    }

    let SandboxAssessmentRequest {
        config,
        provider,
        auth_manager,
        otel_event_manager,
        conversation_id,
        call_id,
        command,
        sandbox_policy,
        cwd,
        failure_message,
    } = request;

    let auth_manager = auth_manager?;

    assess_command(
        config,
        provider,
        auth_manager,
        &otel_event_manager,
        conversation_id,
        &call_id,
        &command,
        &sandbox_policy,
        &cwd,
        failure_message.as_deref(),
    )
    .await
}

#[cfg(test)]
static TEST_OVERRIDE: Lazy<Mutex<Option<SandboxCommandAssessment>>> =
    Lazy::new(|| Mutex::new(None));
