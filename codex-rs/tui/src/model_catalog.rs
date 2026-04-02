use codex_core::CLAUDE_CLI_PROVIDER_ID;
use codex_core::OPENAI_PROVIDER_ID;
use codex_core::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use codex_core::models_manager::collaboration_mode_presets::builtin_collaboration_mode_presets;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::openai_models::ModelPreset;
use std::collections::BTreeMap;
use std::convert::Infallible;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModelCatalogEntry {
    pub(crate) provider_id: String,
    pub(crate) provider_name: String,
    pub(crate) preset: ModelPreset,
}

impl From<ModelPreset> for ModelCatalogEntry {
    fn from(preset: ModelPreset) -> Self {
        Self {
            provider_id: String::new(),
            provider_name: String::new(),
            preset,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModelProviderGroup {
    pub(crate) provider_id: String,
    pub(crate) label: String,
    pub(crate) entries: Vec<ModelCatalogEntry>,
}

pub(crate) fn provider_group_label(entry: &ModelCatalogEntry) -> String {
    match entry.provider_id.as_str() {
        CLAUDE_CLI_PROVIDER_ID => "Anthropic".to_string(),
        OPENAI_PROVIDER_ID => "OpenAI".to_string(),
        _ if !entry.provider_name.trim().is_empty() => entry.provider_name.trim().to_string(),
        _ if !entry.provider_id.trim().is_empty() => entry.provider_id.clone(),
        _ => "Other".to_string(),
    }
}

pub(crate) fn group_picker_models_by_provider(
    entries: Vec<ModelCatalogEntry>,
) -> Vec<ModelProviderGroup> {
    let mut groups: BTreeMap<(String, String), Vec<ModelCatalogEntry>> = BTreeMap::new();

    for entry in entries {
        let label = provider_group_label(&entry);
        groups
            .entry((label, entry.provider_id.clone()))
            .or_default()
            .push(entry);
    }

    groups
        .into_iter()
        .map(|((label, provider_id), entries)| ModelProviderGroup {
            provider_id,
            label,
            entries,
        })
        .collect()
}

#[derive(Debug, Clone)]
pub(crate) struct ModelCatalog {
    models: Vec<ModelCatalogEntry>,
    collaboration_modes_config: CollaborationModesConfig,
}

impl ModelCatalog {
    pub(crate) fn new<T>(
        models: Vec<T>,
        collaboration_modes_config: CollaborationModesConfig,
    ) -> Self
    where
        T: Into<ModelCatalogEntry>,
    {
        Self {
            models: models.into_iter().map(Into::into).collect(),
            collaboration_modes_config,
        }
    }

    pub(crate) fn try_list_models_for_provider(
        &self,
        provider_id: &str,
    ) -> Result<Vec<ModelPreset>, Infallible> {
        let models = self
            .models
            .iter()
            .filter(|entry| entry.provider_id == provider_id)
            .map(|entry| entry.preset.clone())
            .collect::<Vec<_>>();
        if models.is_empty() {
            return Ok(self
                .models
                .iter()
                .filter(|entry| entry.provider_id.is_empty())
                .map(|entry| entry.preset.clone())
                .collect());
        }
        Ok(models)
    }

    pub(crate) fn try_list_picker_models(&self) -> Result<Vec<ModelCatalogEntry>, Infallible> {
        Ok(self.models.clone())
    }

    pub(crate) fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        builtin_collaboration_mode_presets(self.collaboration_modes_config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn preset(model: &str) -> ModelPreset {
        ModelPreset {
            id: model.to_string(),
            model: model.to_string(),
            display_name: model.to_string(),
            description: String::new(),
            default_reasoning_effort: codex_protocol::openai_models::ReasoningEffort::Medium,
            supported_reasoning_efforts: Vec::new(),
            supports_personality: false,
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            availability_nux: None,
            supported_in_api: true,
            input_modalities: Vec::new(),
        }
    }

    #[test]
    fn list_collaboration_modes_matches_core_presets() {
        let collaboration_modes_config = CollaborationModesConfig {
            default_mode_request_user_input: true,
        };
        let catalog = ModelCatalog::new(Vec::<ModelPreset>::new(), collaboration_modes_config);

        assert_eq!(
            catalog.list_collaboration_modes(),
            builtin_collaboration_mode_presets(collaboration_modes_config)
        );
    }

    #[test]
    fn provider_group_label_maps_built_in_provider_families() {
        assert_eq!(
            provider_group_label(&ModelCatalogEntry {
                provider_id: CLAUDE_CLI_PROVIDER_ID.to_string(),
                provider_name: "Claude Code CLI".to_string(),
                preset: preset("claude-opus-4-6"),
            }),
            "Anthropic"
        );
        assert_eq!(
            provider_group_label(&ModelCatalogEntry {
                provider_id: OPENAI_PROVIDER_ID.to_string(),
                provider_name: "OpenAI".to_string(),
                preset: preset("gpt-5.4"),
            }),
            "OpenAI"
        );
    }

    #[test]
    fn groups_picker_models_by_provider_label() {
        let groups = group_picker_models_by_provider(vec![
            ModelCatalogEntry {
                provider_id: OPENAI_PROVIDER_ID.to_string(),
                provider_name: "OpenAI".to_string(),
                preset: preset("gpt-5.4"),
            },
            ModelCatalogEntry {
                provider_id: CLAUDE_CLI_PROVIDER_ID.to_string(),
                provider_name: "Claude Code CLI".to_string(),
                preset: preset("claude-opus-4-6"),
            },
            ModelCatalogEntry {
                provider_id: CLAUDE_CLI_PROVIDER_ID.to_string(),
                provider_name: "Claude Code CLI".to_string(),
                preset: preset("claude-sonnet-4-6"),
            },
        ]);

        assert_eq!(
            groups,
            vec![
                ModelProviderGroup {
                    provider_id: CLAUDE_CLI_PROVIDER_ID.to_string(),
                    label: "Anthropic".to_string(),
                    entries: vec![
                        ModelCatalogEntry {
                            provider_id: CLAUDE_CLI_PROVIDER_ID.to_string(),
                            provider_name: "Claude Code CLI".to_string(),
                            preset: preset("claude-opus-4-6"),
                        },
                        ModelCatalogEntry {
                            provider_id: CLAUDE_CLI_PROVIDER_ID.to_string(),
                            provider_name: "Claude Code CLI".to_string(),
                            preset: preset("claude-sonnet-4-6"),
                        },
                    ],
                },
                ModelProviderGroup {
                    provider_id: OPENAI_PROVIDER_ID.to_string(),
                    label: "OpenAI".to_string(),
                    entries: vec![ModelCatalogEntry {
                        provider_id: OPENAI_PROVIDER_ID.to_string(),
                        provider_name: "OpenAI".to_string(),
                        preset: preset("gpt-5.4"),
                    }],
                },
            ]
        );
    }
}
