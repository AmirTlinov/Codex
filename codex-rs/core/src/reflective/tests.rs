use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use pretty_assertions::assert_eq;

fn sample_report(disposition: model::ReflectiveDisposition) -> prompt::ReflectiveReport {
    prompt::ReflectiveReport {
        observations: vec![model::ReflectiveObservation {
            category: model::ReflectiveObservationCategory::Risk,
            note: "Check non-obvious race".to_string(),
            why_it_matters: "Late maintenance results can overwrite a newer truth".to_string(),
            evidence: "Result application happens asynchronously after the main turn".to_string(),
            confidence: model::ReflectiveConfidence::High,
            disposition,
        }],
    }
}

#[test]
fn should_schedule_after_regular_turn_requires_signal() {
    assert!(!should_schedule_after_regular_turn(
        /*feature_enabled*/ true,
        &SessionSource::Cli,
        /*turn_tool_calls*/ 0,
        /*turn_total_tokens*/ 10,
        Some("done"),
    ));
    assert!(should_schedule_after_regular_turn(
        /*feature_enabled*/ true,
        &SessionSource::Cli,
        /*turn_tool_calls*/ 1,
        /*turn_total_tokens*/ 10,
        Some("done"),
    ));
    assert!(should_schedule_after_regular_turn(
        /*feature_enabled*/ true,
        &SessionSource::Cli,
        /*turn_tool_calls*/ 0,
        /*turn_total_tokens*/ 3_500,
        Some("done"),
    ));
    assert!(!should_schedule_after_regular_turn(
        /*feature_enabled*/ false,
        &SessionSource::Cli,
        /*turn_tool_calls*/ 1,
        /*turn_total_tokens*/ 3_500,
        Some("done"),
    ));
}

#[test]
fn reflective_window_drops_discarded_observations() {
    let window = ReflectiveWindowState::from_report(
        "turn-1".to_string(),
        sample_report(model::ReflectiveDisposition::Discard),
    );
    assert_eq!(window, None);
}

#[test]
fn reflective_window_into_prompt_item_uses_reflective_fragment() {
    let window = ReflectiveWindowState::from_report(
        "turn-1".to_string(),
        sample_report(model::ReflectiveDisposition::Verify),
    )
    .expect("window");

    let item = window.into_prompt_item();
    let ResponseItem::Message { content, .. } = item else {
        panic!("expected message");
    };
    let [ContentItem::InputText { text }] = content.as_slice() else {
        panic!("expected input text");
    };
    assert!(text.contains("<reflective_window>"));
    assert!(text.contains("Check non-obvious race"));
}
