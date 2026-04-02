use std::sync::Arc;

use codex_app_server_protocol::Model;
use codex_app_server_protocol::ModelUpgradeInfo;
use codex_app_server_protocol::ReasoningEffortOption;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_core::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use codex_core::models_manager::manager::ModelsManager;
use codex_core::models_manager::manager::RefreshStrategy;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffortPreset;

pub async fn supported_models(
    config: &Config,
    thread_manager: Arc<ThreadManager>,
    include_hidden: bool,
) -> Result<Vec<Model>, String> {
    supported_models_for_providers(
        config,
        thread_manager,
        include_hidden,
        /*providers*/ None,
    )
    .await
}

pub async fn supported_models_for_providers(
    config: &Config,
    thread_manager: Arc<ThreadManager>,
    include_hidden: bool,
    providers: Option<&[String]>,
) -> Result<Vec<Model>, String> {
    let provider_ids = requested_provider_ids(config, providers);
    let mut models = Vec::new();

    for provider_id in provider_ids {
        let (provider_name, presets) =
            presets_for_provider(config, Arc::clone(&thread_manager), provider_id.as_str()).await?;

        models.extend(
            presets
                .into_iter()
                .filter(|preset| include_hidden || preset.show_in_picker)
                .map(|preset| {
                    model_from_preset(provider_id.as_str(), provider_name.as_str(), preset)
                }),
        );
    }

    Ok(models)
}

pub async fn validate_model_selection(
    config: &Config,
    thread_manager: Arc<ThreadManager>,
    provider_id: &str,
    model: &str,
) -> Result<(), String> {
    let (_, presets) = presets_for_provider(config, thread_manager, provider_id).await?;
    if presets.iter().any(|preset| preset.model == model) {
        return Ok(());
    }

    let allowed_models = presets
        .iter()
        .map(|preset| preset.model.clone())
        .collect::<Vec<_>>();
    let allowed = if allowed_models.is_empty() {
        format!("models available from provider `{provider_id}`: [none]")
    } else {
        format!(
            "models available from provider `{provider_id}`: {}",
            allowed_models.join(", ")
        )
    };
    Err(format!(
        "invalid value for `model`: `{model}` is not in the allowed set {allowed} (set by unknown)"
    ))
}

async fn presets_for_provider(
    config: &Config,
    thread_manager: Arc<ThreadManager>,
    provider_id: &str,
) -> Result<(String, Vec<ModelPreset>), String> {
    let Some(provider) = config.model_providers.get(provider_id) else {
        return Err(format!("unknown model provider: {provider_id}"));
    };

    let refresh_strategy = if provider_id == config.model_provider_id {
        RefreshStrategy::OnlineIfUncached
    } else {
        RefreshStrategy::Offline
    };
    let presets = ModelsManager::new_with_provider(
        config.codex_home.clone(),
        thread_manager.auth_manager(),
        model_catalog_for_provider(config, provider_id),
        CollaborationModesConfig {
            default_mode_request_user_input: false,
        },
        provider.clone(),
    )
    .list_models(refresh_strategy)
    .await;

    Ok((provider.name.clone(), presets))
}

fn model_catalog_for_provider(config: &Config, provider_id: &str) -> Option<ModelsResponse> {
    (provider_id == config.model_provider_id)
        .then(|| config.model_catalog.clone())
        .flatten()
}

fn requested_provider_ids(config: &Config, providers: Option<&[String]>) -> Vec<String> {
    if let Some(providers) = providers
        && !providers.is_empty()
    {
        let mut unique = Vec::new();
        for provider_id in providers {
            if !unique.contains(provider_id) {
                unique.push(provider_id.clone());
            }
        }
        return unique;
    }

    vec![config.model_provider_id.clone()]
}

fn model_from_preset(provider_id: &str, provider_name: &str, preset: ModelPreset) -> Model {
    Model {
        id: preset.id.to_string(),
        model: preset.model.to_string(),
        provider_id: provider_id.to_string(),
        provider_name: provider_name.to_string(),
        upgrade: preset.upgrade.as_ref().map(|upgrade| upgrade.id.clone()),
        upgrade_info: preset.upgrade.as_ref().map(|upgrade| ModelUpgradeInfo {
            model: upgrade.id.clone(),
            upgrade_copy: upgrade.upgrade_copy.clone(),
            model_link: upgrade.model_link.clone(),
            migration_markdown: upgrade.migration_markdown.clone(),
        }),
        availability_nux: preset.availability_nux.map(Into::into),
        display_name: preset.display_name.to_string(),
        description: preset.description.to_string(),
        hidden: !preset.show_in_picker,
        supported_reasoning_efforts: reasoning_efforts_from_preset(
            preset.supported_reasoning_efforts,
        ),
        default_reasoning_effort: preset.default_reasoning_effort,
        input_modalities: preset.input_modalities,
        supports_personality: preset.supports_personality,
        is_default: preset.is_default,
    }
}

fn reasoning_efforts_from_preset(
    efforts: Vec<ReasoningEffortPreset>,
) -> Vec<ReasoningEffortOption> {
    efforts
        .iter()
        .map(|preset| ReasoningEffortOption {
            reasoning_effort: preset.effort,
            description: preset.description.to_string(),
        })
        .collect()
}
