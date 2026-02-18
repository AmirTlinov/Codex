use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::config_types::ModeKind;
use codex_protocol::openai_models::ReasoningEffort;

const COLLABORATION_MODE_PLAN: &str = include_str!("../../templates/collaboration_mode/plan.md");
const COLLABORATION_MODE_DEFAULT: &str =
    include_str!("../../templates/collaboration_mode/default.md");
const COLLABORATION_MODE_ORCHESTRATOR: &str =
    include_str!("../../templates/collaboration_mode/orchestrator.md");
const ORCHESTRATOR_MODE_NAME: &str = "Orchestrator";
const KNOWN_MODE_NAMES: [&str; 3] = [
    ModeKind::Default.display_name(),
    ORCHESTRATOR_MODE_NAME,
    ModeKind::Plan.display_name(),
];
const KNOWN_MODE_NAMES_PLACEHOLDER: &str = "{{KNOWN_MODE_NAMES}}";
const REQUEST_USER_INPUT_AVAILABILITY_PLACEHOLDER: &str = "{{REQUEST_USER_INPUT_AVAILABILITY}}";

pub(crate) fn builtin_collaboration_mode_presets() -> Vec<CollaborationModeMask> {
    vec![default_preset(), orchestrator_preset(), plan_preset()]
}

fn plan_preset() -> CollaborationModeMask {
    CollaborationModeMask {
        name: ModeKind::Plan.display_name().to_string(),
        mode: Some(ModeKind::Plan),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(COLLABORATION_MODE_PLAN.to_string())),
    }
}

fn default_preset() -> CollaborationModeMask {
    CollaborationModeMask {
        name: ModeKind::Default.display_name().to_string(),
        mode: Some(ModeKind::Default),
        model: None,
        reasoning_effort: None,
        developer_instructions: Some(Some(default_mode_instructions())),
    }
}

fn orchestrator_preset() -> CollaborationModeMask {
    CollaborationModeMask {
        name: ORCHESTRATOR_MODE_NAME.to_string(),
        mode: Some(ModeKind::Default),
        model: None,
        reasoning_effort: None,
        developer_instructions: Some(Some(orchestrator_mode_instructions())),
    }
}

fn default_mode_instructions() -> String {
    let known_mode_names = format_mode_names(&KNOWN_MODE_NAMES);
    let request_user_input_availability =
        request_user_input_availability_message(ModeKind::Default);
    COLLABORATION_MODE_DEFAULT
        .replace(KNOWN_MODE_NAMES_PLACEHOLDER, &known_mode_names)
        .replace(
            REQUEST_USER_INPUT_AVAILABILITY_PLACEHOLDER,
            &request_user_input_availability,
        )
}

fn orchestrator_mode_instructions() -> String {
    let request_user_input_availability =
        request_user_input_availability_message(ModeKind::Default);
    COLLABORATION_MODE_ORCHESTRATOR.replace(
        REQUEST_USER_INPUT_AVAILABILITY_PLACEHOLDER,
        &request_user_input_availability,
    )
}

fn format_mode_names(mode_names: &[&str]) -> String {
    match mode_names {
        [] => "none".to_string(),
        [mode_name] => (*mode_name).to_string(),
        [first, second] => format!("{first} and {second}"),
        [..] => mode_names.join(", "),
    }
}

fn request_user_input_availability_message(mode: ModeKind) -> String {
    let mode_name = mode.display_name();
    if mode.allows_request_user_input() {
        format!("The `request_user_input` tool is available in {mode_name} mode.")
    } else {
        format!(
            "The `request_user_input` tool is unavailable in {mode_name} mode. If you call it while in {mode_name} mode, it will return an error."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn preset_names_use_mode_display_names() {
        assert_eq!(plan_preset().name, ModeKind::Plan.display_name());
        assert_eq!(default_preset().name, ModeKind::Default.display_name());
        assert_eq!(orchestrator_preset().name, ORCHESTRATOR_MODE_NAME);
    }

    #[test]
    fn builtin_presets_are_returned_in_tui_cycling_order() {
        let preset_names: Vec<String> = builtin_collaboration_mode_presets()
            .into_iter()
            .map(|preset| preset.name)
            .collect();
        assert_eq!(
            preset_names,
            vec![
                ModeKind::Default.display_name().to_string(),
                ORCHESTRATOR_MODE_NAME.to_string(),
                ModeKind::Plan.display_name().to_string(),
            ]
        );
    }

    #[test]
    fn orchestrator_preset_uses_default_mode() {
        assert_eq!(orchestrator_preset().mode, Some(ModeKind::Default));
    }

    #[test]
    fn default_mode_instructions_replace_mode_names_placeholder() {
        let default_instructions = default_preset()
            .developer_instructions
            .expect("default preset should include instructions")
            .expect("default instructions should be set");

        assert!(!default_instructions.contains(KNOWN_MODE_NAMES_PLACEHOLDER));
        assert!(!default_instructions.contains(REQUEST_USER_INPUT_AVAILABILITY_PLACEHOLDER));

        let known_mode_names = format_mode_names(&KNOWN_MODE_NAMES);
        let expected_snippet = format!("Known mode names are {known_mode_names}.");
        assert!(default_instructions.contains(&expected_snippet));

        let expected_availability_message =
            request_user_input_availability_message(ModeKind::Default);
        assert!(default_instructions.contains(&expected_availability_message));
    }

    #[test]
    fn orchestrator_mode_instructions_replace_request_user_input_placeholder() {
        let orchestrator_instructions = orchestrator_preset()
            .developer_instructions
            .expect("orchestrator preset should include instructions")
            .expect("orchestrator instructions should be set");

        assert!(!orchestrator_instructions.contains(REQUEST_USER_INPUT_AVAILABILITY_PLACEHOLDER));

        let expected_availability_message =
            request_user_input_availability_message(ModeKind::Default);
        assert!(orchestrator_instructions.contains(&expected_availability_message));
    }
}
