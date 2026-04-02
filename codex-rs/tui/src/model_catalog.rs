use codex_core::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use codex_core::models_manager::collaboration_mode_presets::builtin_collaboration_mode_presets;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::openai_models::ModelPreset;
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
}
