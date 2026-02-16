use crate::config::Config;
use crate::features::Feature;
use crate::protocol::SandboxPolicy;
use codex_protocol::openai_models::ReasoningEffort;
use serde::Deserialize;
use serde::Serialize;

/// Base instructions for deep context reconnaissance.
const SCOUT_PROMPT: &str = include_str!("../../templates/agents/scout.md");
/// Base instructions for patch generation.
const BUILDER_PROMPT: &str = include_str!("../../templates/agents/builder.md");
/// Base instructions for patch validation.
const VALIDATOR_PROMPT: &str = include_str!("../../templates/agents/validator.md");
/// Base instructions for slice planning.
const PLAN_PROMPT: &str = include_str!("../../templates/agents/plan.md");
const CONTEXT_VALIDATOR_PROMPT: &str = r#"Use `context_validator` to review collected context before Builder starts.
- Validate coverage and consistency.
- Flag missing evidence, ambiguous assumptions, and scope gaps.
- Focus only on context quality and pre-build risk assessment.
- Do not apply or propose code patches."#;
const POST_BUILDER_VALIDATOR_PROMPT: &str = r#"Use `post_builder_validator` to validate Builder patches against the current task.
- Confirm requirements and context were respected.
- Apply accepted Builder patches verbatim.
- Reject unsafe or incorrect patches with concrete, file-specific reasons.
- Do not invent alternative patch text."#;

/// Enumerated list of all supported agent roles.
const ALL_ROLES: [AgentRole; 7] = [
    AgentRole::Default,
    AgentRole::Scout,
    AgentRole::ContextValidator,
    AgentRole::Builder,
    AgentRole::PostBuilderValidator,
    AgentRole::Validator,
    AgentRole::Plan,
];

/// Hard-coded agent role selection used when spawning sub-agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Inherit the parent agent's configuration unchanged.
    #[serde(alias = "main", alias = "orchestrator")]
    Default,
    /// Scout: context-only role.
    #[serde(alias = "explorer")]
    Scout,
    /// Context validator: review gathered context before execution.
    ContextValidator,
    /// Builder: patch-only role.
    #[serde(alias = "worker")]
    Builder,
    /// Post-builder validator: final validation role after patch generation.
    PostBuilderValidator,
    /// Validator: review + acceptance role.
    Validator,
    /// Plan: slice-first planning role.
    #[serde(alias = "planner")]
    Plan,
}

/// Immutable profile data that drives per-agent configuration overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgentProfile {
    /// Optional base instructions override.
    pub base_instructions: Option<&'static str>,
    /// Optional model override.
    pub model: Option<&'static str>,
    /// Optional reasoning effort override.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Whether to force a read-only sandbox policy.
    pub read_only: bool,
    /// Description to include in the tool specs.
    pub description: &'static str,
}

impl AgentRole {
    /// Detect role from effective base instructions.
    pub fn detect_from_config(config: &Config) -> Self {
        match config.base_instructions.as_deref() {
            Some(SCOUT_PROMPT) => AgentRole::Scout,
            Some(CONTEXT_VALIDATOR_PROMPT) => AgentRole::ContextValidator,
            Some(BUILDER_PROMPT) => AgentRole::Builder,
            Some(POST_BUILDER_VALIDATOR_PROMPT) => AgentRole::PostBuilderValidator,
            Some(VALIDATOR_PROMPT) => AgentRole::Validator,
            Some(PLAN_PROMPT) => AgentRole::Plan,
            _ => AgentRole::Default,
        }
    }

    /// Returns the string values used by JSON schema enums.
    pub fn enum_values() -> Vec<String> {
        ALL_ROLES
            .iter()
            .filter_map(|role| {
                let description = role.profile().description;
                serde_json::to_string(&serde_json::json!({
                    "name": role,
                    "description": if description.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(description.to_string())
                    },
                }))
                .ok()
            })
            .collect()
    }

    /// Returns the hard-coded profile for this role.
    pub fn profile(self) -> AgentProfile {
        match self {
            AgentRole::Default => AgentProfile {
                description: r#"Use `default` only as orchestrator and fallback executor.
Rules:
- Prefer delegating by default: `scout` -> `builder` -> `validator`.
- Execute tools directly only when delegation is blocked or has failed.
- Keep direct actions minimal and explicitly justified."#,
                ..Default::default()
            },
            AgentRole::Scout => AgentProfile {
                base_instructions: Some(SCOUT_PROMPT),
                reasoning_effort: Some(ReasoningEffort::Medium),
                read_only: true,
                description: r#"Use `scout` for context discovery and anchor generation.
Goals:
- Build a total, low-noise technical context pack for the requested task.
- Extract stable anchors in code without duplication.
- Provide concise Mermaid diagrams for key flows.
- Return clear uncertainty gaps before patch generation begins.
Rules:
- Do not generate or apply patches.
- You may use any tools needed to gather context, but do not mutate the workspace.
- Do not duplicate anchors or context already provided.
- Favor factual, deterministic findings over guesses."#,
                ..Default::default()
            },
            AgentRole::ContextValidator => AgentProfile {
                base_instructions: Some(CONTEXT_VALIDATOR_PROMPT),
                read_only: true,
                description: r#"Use `context_validator` to validate gathered context before Builder begins.
Goals:
- Confirm task constraints and assumptions are well-supported.
- Identify missing context and uncertainty gaps.
- Recommend minimal additional reconnaissance.
Rules:
- Do not edit files.
- Do not execute shell or approval-heavy tools.
- Keep the output deterministic and evidence-focused."#,
                ..Default::default()
            },
            AgentRole::Builder => AgentProfile {
                base_instructions: Some(BUILDER_PROMPT),
                read_only: true,
                description: r#"Use `builder` only for patch generation.
Typical tasks:
- Produce minimal unified patches from full context.
- Keep changes scoped to requested files.
- Return patches in an incremental, reviewable form.
- If context is missing, explicitly list exact gaps and ask Main to trigger additional scout passes.
Rules:
- Do not execute shell commands, run tests, or perform approvals.
- Do not apply patches directly; only return patch text proposals.
- Do not rewrite already-correct code.
- Do not invent new assumptions when context is insufficient."#,
                ..Default::default()
            },
            AgentRole::PostBuilderValidator => AgentProfile {
                base_instructions: Some(POST_BUILDER_VALIDATOR_PROMPT),
                description: r#"Use `post_builder_validator` to review Builder patches.
Goals:
- Validate final patch correctness against requirements and context.
- Apply accepted Builder patches only verbatim.
- Reject rejected patches with file-specific rationale.
Rules:
- Do not invent new patch text while validating.
- Do not mix review and patch composition.
- Focus on objective validation and risk evidence."#,
                ..Default::default()
            },
            AgentRole::Validator => AgentProfile {
                base_instructions: Some(VALIDATOR_PROMPT),
                description: r#"Use `validator` to review Builder patches against task intent and context.
Typical tasks:
- Confirm patch scope and correctness.
- Reject unsafe or under-justified edits.
- Request incremental Builder updates instead of rewrites when fixes are needed.
- Apply accepted patches verbatim and report resulting state.
Rules:
- Focus on objective evidence from context/history.
- If context is missing, request another `scout` pass via Main rather than guessing.
- Do not rewrite patch text while applying. Apply only verbatim patch input from Builder."#,
                ..Default::default()
            },
            AgentRole::Plan => AgentProfile {
                base_instructions: Some(PLAN_PROMPT),
                description: r#"Use `plan` to generate slice-first implementation plans.
Rules:
- Produce PLAN.md plus executable slice files for scout+builder+validator flow.
- Avoid big-bang plans; each slice must be independently executable.
- Write plans only under ~/.codex/plans/<repo>_<session>/<plan_name>/."#,
                ..Default::default()
            },
        }
    }

    /// Applies this role's profile onto the provided config.
    pub fn apply_to_config(self, config: &mut Config) -> Result<(), String> {
        let profile = self.profile();
        if let Some(model) = self.resolve_model(config) {
            config.model = Some(model);
        }
        self.apply_feature_policy(config);
        if let Some(base_instructions) = profile.base_instructions {
            config.base_instructions = Some(base_instructions.to_string());
        }
        if let Some(reasoning_effort) = profile.reasoning_effort {
            config.model_reasoning_effort = Some(reasoning_effort)
        }
        if profile.read_only {
            config
                .permissions
                .sandbox_policy
                .set(SandboxPolicy::new_read_only_policy())
                .map_err(|err| format!("sandbox_policy is invalid: {err}"))?;
        }
        Ok(())
    }

    fn apply_feature_policy(self, config: &mut Config) {
        match self {
            AgentRole::Default => {}
            AgentRole::ContextValidator => {
                config.features.disable(Feature::ApplyPatchFreeform);
                config.features.disable(Feature::ShellTool);
                config.features.disable(Feature::UnifiedExec);
                config.features.disable(Feature::JsRepl);
                config.features.disable(Feature::JsReplToolsOnly);
                config.features.disable(Feature::Apps);
                config.features.disable(Feature::WebSearchRequest);
                config.features.disable(Feature::WebSearchCached);
                config.features.disable(Feature::Collab);
                config.features.disable(Feature::CollaborationModes);
                config.features.disable(Feature::RequestRule);
            }
            AgentRole::Scout => {
                // Keep the full tool surface for context gathering. Enforcement of scout behavior
                // is handled by tool policy + prompt contract, while the sandbox policy remains
                // read-only.
            }
            AgentRole::Builder => {
                config.features.disable(Feature::ApplyPatchFreeform);
                config.features.disable(Feature::ShellTool);
                config.features.disable(Feature::UnifiedExec);
                config.features.disable(Feature::JsRepl);
                config.features.disable(Feature::JsReplToolsOnly);
                config.features.disable(Feature::Apps);
                config.features.disable(Feature::WebSearchRequest);
                config.features.disable(Feature::WebSearchCached);
                config.features.disable(Feature::Collab);
                config.features.disable(Feature::CollaborationModes);
                config.features.disable(Feature::RequestRule);
            }
            AgentRole::PostBuilderValidator => {
                config.features.enable(Feature::ApplyPatchFreeform);
                config.features.disable(Feature::ShellTool);
                config.features.disable(Feature::UnifiedExec);
                config.features.disable(Feature::Collab);
                config.features.disable(Feature::CollaborationModes);
                config.features.disable(Feature::RequestRule);
            }
            AgentRole::Validator => {
                config.features.enable(Feature::ApplyPatchFreeform);
                config.features.disable(Feature::ShellTool);
                config.features.disable(Feature::UnifiedExec);
                config.features.disable(Feature::Collab);
                config.features.disable(Feature::CollaborationModes);
                config.features.disable(Feature::RequestRule);
            }
            AgentRole::Plan => {
                config.features.enable(Feature::ApplyPatchFreeform);
                config.features.enable(Feature::Collab);
                config.features.enable(Feature::CollaborationModes);
                config.features.disable(Feature::ShellTool);
                config.features.disable(Feature::UnifiedExec);
                config.features.disable(Feature::JsRepl);
                config.features.disable(Feature::JsReplToolsOnly);
                config.features.disable(Feature::Apps);
                config.features.disable(Feature::WebSearchRequest);
                config.features.disable(Feature::WebSearchCached);
                config.features.disable(Feature::RequestRule);
            }
        }
    }

    /// Resolves the preferred model for this role using config overrides.
    ///
    /// Resolution order:
    /// 1. Explicit role model in `[agents]`.
    /// 2. `[agents].main_model`.
    /// 3. General `model` fallback.
    pub fn resolve_model(self, config: &Config) -> Option<String> {
        let role_model = match self {
            AgentRole::Default => config.agents.main_model.clone(),
            AgentRole::Scout => config.agents.scout_model.clone(),
            AgentRole::ContextValidator => config.agents.context_validator_model.clone(),
            AgentRole::Builder => config.agents.builder_model.clone(),
            AgentRole::PostBuilderValidator => config.agents.post_builder_validator_model.clone(),
            AgentRole::Validator => config.agents.validator_model.clone(),
            AgentRole::Plan => config.agents.plan_model.clone(),
        };
        role_model
            .or_else(|| config.agents.main_model.clone())
            .or_else(|| config.model.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::AgentRole;
    use super::BUILDER_PROMPT;
    use super::CONTEXT_VALIDATOR_PROMPT;
    use super::PLAN_PROMPT;
    use super::POST_BUILDER_VALIDATOR_PROMPT;
    use super::SCOUT_PROMPT;
    use super::VALIDATOR_PROMPT;
    use crate::features::Feature;
    use serde_json::json;

    #[test]
    fn enum_values_contains_all_public_roles() {
        let values = AgentRole::enum_values();
        let joined = values.join("\n");

        assert!(joined.contains("\"default\""), "missing default role");
        assert!(joined.contains("\"scout\""), "missing scout role");
        assert!(
            joined.contains("\"context_validator\""),
            "missing context_validator role"
        );
        assert!(joined.contains("\"builder\""), "missing builder role");
        assert!(
            joined.contains("\"post_builder_validator\""),
            "missing post_builder_validator role"
        );
        assert!(joined.contains("\"validator\""), "missing validator role");
        assert!(joined.contains("\"plan\""), "missing plan role");
    }

    #[test]
    fn scout_alias_is_back_compatible() {
        let role: AgentRole = serde_json::from_str(&json!("explorer").to_string())
            .expect("legacy explorer should deserialize as scout");

        assert_eq!(role, AgentRole::Scout);
    }

    #[test]
    fn builder_alias_is_back_compatible() {
        let role: AgentRole = serde_json::from_str(&json!("worker").to_string())
            .expect("legacy worker should deserialize as builder");

        assert_eq!(role, AgentRole::Builder);
    }

    #[test]
    fn role_sandbox_expectations_are_stable() {
        assert!(AgentRole::Scout.profile().read_only);
        assert!(AgentRole::ContextValidator.profile().read_only);
        assert!(AgentRole::Builder.profile().read_only);
        assert!(!AgentRole::Default.profile().read_only);
        assert!(!AgentRole::PostBuilderValidator.profile().read_only);
        assert!(!AgentRole::Validator.profile().read_only);
        assert!(!AgentRole::Plan.profile().read_only);
    }

    #[test]
    fn detect_from_config_uses_role_prompts() {
        let mut config = crate::config::test_config();
        config.base_instructions = Some(SCOUT_PROMPT.to_string());
        assert_eq!(AgentRole::detect_from_config(&config), AgentRole::Scout);

        config.base_instructions = Some(CONTEXT_VALIDATOR_PROMPT.to_string());
        assert_eq!(
            AgentRole::detect_from_config(&config),
            AgentRole::ContextValidator
        );

        config.base_instructions = Some(BUILDER_PROMPT.to_string());
        assert_eq!(AgentRole::detect_from_config(&config), AgentRole::Builder);

        config.base_instructions = Some(POST_BUILDER_VALIDATOR_PROMPT.to_string());
        assert_eq!(
            AgentRole::detect_from_config(&config),
            AgentRole::PostBuilderValidator
        );

        config.base_instructions = Some(VALIDATOR_PROMPT.to_string());
        assert_eq!(AgentRole::detect_from_config(&config), AgentRole::Validator);

        config.base_instructions = Some(PLAN_PROMPT.to_string());
        assert_eq!(AgentRole::detect_from_config(&config), AgentRole::Plan);
    }

    #[test]
    fn scout_prompt_contract_mentions_scout_pack_generator() {
        let required_snippets = [
            "`excerpt_spec.yml`",
            "`context_pack.md`",
            "scripts/scout_pack.py",
            "just scout-pack-check",
            "just scout-pack",
            "CODE_REF::<crate>::",
            ".agents/skills/scout_context_pack/templates/excerpt_spec.example.yml",
            "examples/scout_packs/role_split/excerpt_spec.yml",
        ];

        for snippet in required_snippets {
            assert!(
                SCOUT_PROMPT.contains(snippet),
                "SCOUT_PROMPT missing required snippet: {snippet}"
            );
        }
    }

    #[test]
    fn builder_feature_policy_is_restrictive_and_patch_first() {
        let mut config = crate::config::test_config();
        AgentRole::Builder
            .apply_to_config(&mut config)
            .expect("builder role should apply");

        assert!(!config.features.enabled(Feature::ApplyPatchFreeform));
        assert!(!config.features.enabled(Feature::ShellTool));
        assert!(!config.features.enabled(Feature::Collab));
        assert!(!config.features.enabled(Feature::CollaborationModes));
    }

    #[test]
    fn context_validator_role_disables_non_validation_tools() {
        let mut config = crate::config::test_config();
        AgentRole::ContextValidator
            .apply_to_config(&mut config)
            .expect("context validator role should apply");

        assert!(!config.features.enabled(Feature::ApplyPatchFreeform));
        assert!(!config.features.enabled(Feature::ShellTool));
        assert!(!config.features.enabled(Feature::JsRepl));
        assert!(!config.features.enabled(Feature::Collab));
    }

    #[test]
    fn post_builder_validator_role_allows_patch_and_disables_shell() {
        let mut config = crate::config::test_config();
        AgentRole::PostBuilderValidator
            .apply_to_config(&mut config)
            .expect("post-builder validator role should apply");

        assert!(config.features.enabled(Feature::ApplyPatchFreeform));
        assert!(!config.features.enabled(Feature::ShellTool));
        assert!(!config.features.enabled(Feature::Collab));
    }

    #[test]
    fn plan_role_uses_plan_model_override() {
        let mut config = crate::config::test_config();
        config.model = Some("global-model".to_string());
        config.agents.main_model = Some("main-model".to_string());
        config.agents.plan_model = Some("plan-model".to_string());

        AgentRole::Plan
            .apply_to_config(&mut config)
            .expect("plan role should apply");

        assert_eq!(config.model, Some("plan-model".to_string()));
    }

    #[test]
    fn plan_role_falls_back_to_main_model() {
        let mut config = crate::config::test_config();
        config.model = Some("global-model".to_string());
        config.agents.main_model = Some("main-model".to_string());
        config.agents.plan_model = None;

        AgentRole::Plan
            .apply_to_config(&mut config)
            .expect("plan role should apply");

        assert_eq!(config.model, Some("main-model".to_string()));
    }

    #[test]
    fn context_validator_uses_context_validator_model_override() {
        let mut config = crate::config::test_config();
        config.model = Some("global-model".to_string());
        config.agents.main_model = Some("main-model".to_string());
        config.agents.context_validator_model = Some("context-validator-model".to_string());

        AgentRole::ContextValidator
            .apply_to_config(&mut config)
            .expect("context validator role should apply");

        assert_eq!(config.model, Some("context-validator-model".to_string()));
    }

    #[test]
    fn post_builder_validator_falls_back_to_main_model() {
        let mut config = crate::config::test_config();
        config.model = Some("global-model".to_string());
        config.agents.main_model = Some("main-model".to_string());
        config.agents.post_builder_validator_model = None;

        AgentRole::PostBuilderValidator
            .apply_to_config(&mut config)
            .expect("post-builder validator role should apply");

        assert_eq!(config.model, Some("main-model".to_string()));
    }
}
