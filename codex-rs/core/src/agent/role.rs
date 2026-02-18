use crate::config::Config;
use crate::features::Feature;
use crate::protocol::SandboxPolicy;
use codex_protocol::openai_models::ReasoningEffort;
use serde::Deserialize;
use serde::Serialize;

/// Base instructions for deep context reconnaissance.
const SCOUT_PROMPT: &str = include_str!("../../templates/agents/scout.md");
/// Base instructions for patch validation.
const VALIDATOR_PROMPT: &str = include_str!("../../templates/agents/validator.md");
/// Base instructions for slice planning.
const PLAN_PROMPT: &str = include_str!("../../templates/agents/plan.md");

/// Enumerated list of publicly advertised agent roles.
const ALL_ROLES: [AgentRole; 4] = [
    AgentRole::Default,
    AgentRole::Scout,
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
- Prefer scout-first orchestration: `scout` -> `specialists` -> `validator`.
- Team members may coordinate directly and request their own `scout` passes.
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
            AgentRole::Validator => AgentProfile {
                base_instructions: Some(VALIDATOR_PROMPT),
                description: r#"Use `validator` to review patch packages against task intent and context.
Typical tasks:
- Confirm patch scope and correctness.
- Reject unsafe or under-justified edits with concrete evidence.
- Request focused fixes when changes are incomplete.
- Apply accepted patches and report resulting state.
Rules:
- Focus on objective evidence from context/history.
- If context is missing, request another `scout` pass via Main rather than guessing.
- Do not rewrite large unrelated areas while validating."#,
                ..Default::default()
            },
            AgentRole::Plan => AgentProfile {
                base_instructions: Some(PLAN_PROMPT),
                description: r#"Use `plan` to generate slice-first implementation plans.
Rules:
- Produce PLAN.md plus executable slice files for scout+team orchestration flow.
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
            AgentRole::Scout => {
                // Keep the full tool surface for context gathering. Enforcement of scout behavior
                // is handled by tool policy + prompt contract, while the sandbox policy remains
                // read-only.
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
    use super::PLAN_PROMPT;
    use super::SCOUT_PROMPT;
    use super::VALIDATOR_PROMPT;
    use crate::features::Feature;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn enum_values_contains_all_public_roles() {
        let values = AgentRole::enum_values();
        let role_names = values
            .into_iter()
            .map(|value| {
                let payload: serde_json::Value =
                    serde_json::from_str(&value).expect("agent role enum payload must be JSON");
                payload["name"]
                    .as_str()
                    .expect("agent role enum payload must include name")
                    .to_string()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            role_names,
            vec![
                "default".to_string(),
                "scout".to_string(),
                "validator".to_string(),
                "plan".to_string(),
            ]
        );
    }

    #[test]
    fn scout_alias_is_back_compatible() {
        let role: AgentRole = serde_json::from_str(&json!("explorer").to_string())
            .expect("legacy explorer should deserialize as scout");

        assert_eq!(role, AgentRole::Scout);
    }

    #[test]
    fn unknown_roles_are_rejected() {
        for role in ["custom-role", "ops"] {
            let result = serde_json::from_str::<AgentRole>(&json!(role).to_string());
            assert!(result.is_err(), "unknown role {role} should be rejected");
        }
    }

    #[test]
    fn role_sandbox_expectations_are_stable() {
        assert!(AgentRole::Scout.profile().read_only);
        assert!(!AgentRole::Default.profile().read_only);
        assert!(!AgentRole::Validator.profile().read_only);
        assert!(!AgentRole::Plan.profile().read_only);
    }

    #[test]
    fn detect_from_config_uses_role_prompts() {
        let mut config = crate::config::test_config();
        config.base_instructions = Some(SCOUT_PROMPT.to_string());
        assert_eq!(AgentRole::detect_from_config(&config), AgentRole::Scout);

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
            "version: 2",
            "inline all three artifacts",
            "just scout-pack <excerpt_spec.yml> -o -",
            "Evidence quotes (verbatim, from `context_pack.md`)",
            "CODE_REF without a quote is invalid",
            "placeholder anchors (`Lx-Ly`, `<start>-<end>`, `...`)",
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
    fn default_role_description_enforces_scout_team_pipeline() {
        let description = AgentRole::Default.profile().description;
        let required_snippets = [
            "`scout` -> `specialists` -> `validator`",
            "Team members may coordinate directly and request their own `scout` passes.",
            "Execute tools directly only when delegation is blocked or has failed.",
        ];

        for snippet in required_snippets {
            assert!(
                description.contains(snippet),
                "default role description missing required snippet: {snippet}"
            );
        }
    }

    #[test]
    fn base_prompt_mentions_scout_team_orchestration_flow() {
        const BASE_PROMPT: &str = include_str!("../../prompt.md");
        let required_snippets = [
            "Default orchestration pipeline: `scout` -> `specialist_team` -> `validator`.",
            "No verify/review => not done.",
        ];

        for snippet in required_snippets {
            assert!(
                BASE_PROMPT.contains(snippet),
                "BASE_PROMPT missing required snippet: {snippet}"
            );
        }
    }

    #[test]
    fn validator_feature_policy_allows_patch_and_disables_shell() {
        let mut config = crate::config::test_config();
        AgentRole::Validator
            .apply_to_config(&mut config)
            .expect("validator role should apply");

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
    fn validator_falls_back_to_main_model() {
        let mut config = crate::config::test_config();
        config.model = Some("global-model".to_string());
        config.agents.main_model = Some("main-model".to_string());
        config.agents.validator_model = None;

        AgentRole::Validator
            .apply_to_config(&mut config)
            .expect("validator role should apply");

        assert_eq!(config.model, Some("main-model".to_string()));
    }

    #[test]
    fn scout_uses_scout_model_override() {
        let mut config = crate::config::test_config();
        config.model = Some("global-model".to_string());
        config.agents.main_model = Some("main-model".to_string());
        config.agents.scout_model = Some("scout-model".to_string());

        AgentRole::Scout
            .apply_to_config(&mut config)
            .expect("scout role should apply");

        assert_eq!(config.model, Some("scout-model".to_string()));
    }
}
