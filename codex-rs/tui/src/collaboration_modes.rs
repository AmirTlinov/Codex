use codex_core::models_manager::manager::ModelsManager;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::config_types::ModeKind;

fn filtered_presets(models_manager: &ModelsManager) -> Vec<CollaborationModeMask> {
    models_manager
        .list_collaboration_modes()
        .into_iter()
        .filter(|mask| mask.mode.is_some_and(ModeKind::is_tui_visible))
        .collect()
}

fn default_mask_from_presets(presets: &[CollaborationModeMask]) -> Option<CollaborationModeMask> {
    presets
        .iter()
        .find(|mask| mask.name == ModeKind::Default.display_name())
        .cloned()
        .or_else(|| {
            presets
                .iter()
                .find(|mask| mask.mode == Some(ModeKind::Default))
                .cloned()
        })
        .or_else(|| presets.first().cloned())
}

fn current_preset_index(
    presets: &[CollaborationModeMask],
    current: &CollaborationModeMask,
) -> Option<usize> {
    presets
        .iter()
        .position(|mask| mask == current)
        .or_else(|| presets.iter().position(|mask| mask.name == current.name))
}

fn next_mask_from_presets(
    presets: &[CollaborationModeMask],
    current: Option<&CollaborationModeMask>,
) -> Option<CollaborationModeMask> {
    if presets.is_empty() {
        return None;
    }
    let next_index = current
        .and_then(|current_mask| current_preset_index(presets, current_mask))
        .map_or(0, |idx| (idx + 1) % presets.len());
    presets.get(next_index).cloned()
}

pub(crate) fn presets_for_tui(models_manager: &ModelsManager) -> Vec<CollaborationModeMask> {
    filtered_presets(models_manager)
}

pub(crate) fn default_mask(models_manager: &ModelsManager) -> Option<CollaborationModeMask> {
    default_mask_from_presets(&filtered_presets(models_manager))
}

pub(crate) fn mask_for_kind(
    models_manager: &ModelsManager,
    kind: ModeKind,
) -> Option<CollaborationModeMask> {
    if !kind.is_tui_visible() {
        return None;
    }
    filtered_presets(models_manager)
        .into_iter()
        .find(|mask| mask.mode == Some(kind))
}

/// Cycle to the next collaboration mode preset in list order.
pub(crate) fn next_mask(
    models_manager: &ModelsManager,
    current: Option<&CollaborationModeMask>,
) -> Option<CollaborationModeMask> {
    next_mask_from_presets(&filtered_presets(models_manager), current)
}

pub(crate) fn default_mode_mask(models_manager: &ModelsManager) -> Option<CollaborationModeMask> {
    default_mask(models_manager)
}

pub(crate) fn plan_mask(models_manager: &ModelsManager) -> Option<CollaborationModeMask> {
    mask_for_kind(models_manager, ModeKind::Plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn mask(name: &str, mode: Option<ModeKind>) -> CollaborationModeMask {
        CollaborationModeMask {
            name: name.to_string(),
            mode,
            model: None,
            reasoning_effort: None,
            developer_instructions: None,
        }
    }

    #[test]
    fn default_mask_prefers_named_default_with_multiple_default_modes() {
        let presets = vec![
            mask("Orchestrator", Some(ModeKind::Default)),
            mask("Default", Some(ModeKind::Default)),
            mask("Plan", Some(ModeKind::Plan)),
        ];

        let selected = default_mask_from_presets(&presets).expect("default preset");
        assert_eq!(selected.name, "Default");
    }

    #[test]
    fn next_mask_cycles_by_active_preset_name_when_mode_matches() {
        let presets = vec![
            mask("Default", Some(ModeKind::Default)),
            mask("Orchestrator", Some(ModeKind::Default)),
            mask("Plan", Some(ModeKind::Plan)),
        ];

        let mut orchestrator = mask("Orchestrator", Some(ModeKind::Default));
        orchestrator.model = Some("gpt-5-codex".to_string());

        let next = next_mask_from_presets(&presets, Some(&orchestrator)).expect("next preset");
        assert_eq!(next.name, "Plan");
    }
}
