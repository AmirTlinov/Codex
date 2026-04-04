use super::*;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use pretty_assertions::assert_eq;
use serde_json::json;

fn model_preset(id: &str, show_in_picker: bool) -> ModelPreset {
    ModelPreset {
        id: id.to_string(),
        model: format!("{id}-model"),
        display_name: format!("{id} display"),
        description: format!("{id} description"),
        default_reasoning_effort: ReasoningEffort::Medium,
        supported_reasoning_efforts: vec![ReasoningEffortPreset {
            effort: ReasoningEffort::Medium,
            description: "Balanced".to_string(),
        }],
        supports_personality: false,
        is_default: false,
        upgrade: None,
        show_in_picker,
        availability_nux: None,
        supported_in_api: true,
        input_modalities: Vec::new(),
    }
}

fn claude_opus_preset() -> ModelPreset {
    ModelPreset {
        id: "claude-opus-4-6".to_string(),
        model: "claude-opus-4-6".to_string(),
        display_name: "Claude Opus 4.6".to_string(),
        description: "Claude flagship".to_string(),
        default_reasoning_effort: ReasoningEffort::High,
        supported_reasoning_efforts: vec![
            ReasoningEffortPreset {
                effort: ReasoningEffort::Low,
                description: "Fast".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::Medium,
                description: "Balanced".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::High,
                description: "Deep".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::XHigh,
                description: "Maximum effort".to_string(),
            },
        ],
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
fn spawn_agent_tool_v2_requires_task_name_and_lists_visible_models() {
    let tool = create_spawn_agent_tool_v2(SpawnAgentToolOptions {
        available_models: &[
            model_preset("visible", /*show_in_picker*/ true),
            model_preset("hidden", /*show_in_picker*/ false),
        ],
        agent_type_description: "role help".to_string(),
    });

    let ToolSpec::Function(ResponsesApiTool {
        description,
        parameters,
        output_schema,
        ..
    }) = tool
    else {
        panic!("spawn_agent should be a function tool");
    };
    let JsonSchema::Object {
        properties,
        required,
        ..
    } = parameters
    else {
        panic!("spawn_agent should use object params");
    };
    assert!(description.contains("visible display (`visible-model`)"));
    assert!(!description.contains("hidden display (`hidden-model`)"));
    assert!(description.contains(
        "If you omit `model_provider`, Codex searches the current spawn inventory shown below."
    ));
    assert!(properties.contains_key("task_name"));
    assert!(properties.contains_key("message"));
    assert!(properties.contains_key("fork_turns"));
    assert!(properties.contains_key("model_provider"));
    assert!(!properties.contains_key("items"));
    assert!(!properties.contains_key("fork_context"));
    assert_eq!(
        properties.get("agent_type"),
        Some(&JsonSchema::String {
            description: Some("role help".to_string()),
        })
    );
    assert_eq!(
        required,
        Some(vec!["task_name".to_string(), "message".to_string()])
    );
    assert_eq!(
        output_schema.expect("spawn_agent output schema")["required"],
        json!(["agent_id", "task_name", "nickname"])
    );
}

#[test]
fn spawn_agent_tool_v2_uses_claude_max_label_and_none_for_effortless_models() {
    let mut haiku = model_preset("haiku", /*show_in_picker*/ true);
    haiku.display_name = "Claude Haiku 4.5".to_string();
    haiku.model = "haiku".to_string();
    haiku.supported_reasoning_efforts = Vec::new();
    haiku.default_reasoning_effort = ReasoningEffort::Low;

    let tool = create_spawn_agent_tool_v2(SpawnAgentToolOptions {
        available_models: &[claude_opus_preset(), haiku],
        agent_type_description: "role help".to_string(),
    });

    let ToolSpec::Function(ResponsesApiTool { description, .. }) = tool else {
        panic!("spawn_agent should be a function tool");
    };

    assert!(description.contains("Claude Opus 4.6 (`claude-opus-4-6`)"));
    assert!(description.contains("Supported reasoning efforts: low (Fast), medium (Balanced), high (Deep), max (Maximum effort)."));
    assert!(description.contains("Claude Haiku 4.5 (`haiku`)"));
    assert!(description.contains("Claude Haiku 4.5 (`haiku`): haiku description Default reasoning effort: low. Supported reasoning efforts: none."));
}

#[test]
fn spawn_agent_tool_v1_keeps_legacy_fork_context_field() {
    let tool = create_spawn_agent_tool_v1(SpawnAgentToolOptions {
        available_models: &[],
        agent_type_description: "role help".to_string(),
    });

    let ToolSpec::Function(ResponsesApiTool { parameters, .. }) = tool else {
        panic!("spawn_agent should be a function tool");
    };
    let JsonSchema::Object { properties, .. } = parameters else {
        panic!("spawn_agent should use object params");
    };

    assert!(properties.contains_key("fork_context"));
    assert!(properties.contains_key("model_provider"));
    assert!(!properties.contains_key("fork_turns"));
}

#[test]
fn send_message_tool_requires_message_and_uses_submission_output() {
    let ToolSpec::Function(ResponsesApiTool {
        parameters,
        output_schema,
        ..
    }) = create_send_message_tool()
    else {
        panic!("send_message should be a function tool");
    };
    let JsonSchema::Object {
        properties,
        required,
        ..
    } = parameters
    else {
        panic!("send_message should use object params");
    };
    assert!(properties.contains_key("target"));
    assert!(properties.contains_key("message"));
    assert!(!properties.contains_key("interrupt"));
    assert!(!properties.contains_key("items"));
    assert_eq!(
        required,
        Some(vec!["target".to_string(), "message".to_string()])
    );
    assert_eq!(
        output_schema.expect("send_message output schema")["required"],
        json!(["submission_id"])
    );
}

#[test]
fn assign_task_tool_requires_message_and_uses_submission_output() {
    let ToolSpec::Function(ResponsesApiTool {
        parameters,
        output_schema,
        ..
    }) = create_assign_task_tool()
    else {
        panic!("assign_task should be a function tool");
    };
    let JsonSchema::Object {
        properties,
        required,
        ..
    } = parameters
    else {
        panic!("assign_task should use object params");
    };
    assert!(properties.contains_key("target"));
    assert!(properties.contains_key("message"));
    assert!(properties.contains_key("interrupt"));
    assert!(!properties.contains_key("items"));
    assert_eq!(
        required,
        Some(vec!["target".to_string(), "message".to_string()])
    );
    assert_eq!(
        output_schema.expect("assign_task output schema")["required"],
        json!(["submission_id"])
    );
}

#[test]
fn wait_agent_tool_v2_uses_timeout_only_summary_output() {
    let ToolSpec::Function(ResponsesApiTool {
        parameters,
        output_schema,
        ..
    }) = create_wait_agent_tool_v2(WaitAgentTimeoutOptions {
        default_timeout_ms: 30_000,
        min_timeout_ms: 10_000,
        max_timeout_ms: 3_600_000,
    })
    else {
        panic!("wait_agent should be a function tool");
    };
    let JsonSchema::Object {
        properties,
        required,
        ..
    } = parameters
    else {
        panic!("wait_agent should use object params");
    };
    assert!(!properties.contains_key("targets"));
    assert!(properties.contains_key("timeout_ms"));
    assert_eq!(required, None);
    assert_eq!(
        output_schema.expect("wait output schema")["properties"]["message"]["description"],
        json!("Brief wait summary without the agent's final content.")
    );
}

#[test]
fn list_agents_tool_includes_path_prefix_and_agent_fields() {
    let ToolSpec::Function(ResponsesApiTool {
        parameters,
        output_schema,
        ..
    }) = create_list_agents_tool()
    else {
        panic!("list_agents should be a function tool");
    };
    let JsonSchema::Object { properties, .. } = parameters else {
        panic!("list_agents should use object params");
    };
    assert!(properties.contains_key("path_prefix"));
    assert_eq!(
        output_schema.expect("list_agents output schema")["properties"]["agents"]["items"]["required"],
        json!(["agent_name", "agent_status", "last_task_message"])
    );
}

#[test]
fn list_agents_tool_status_schema_includes_interrupted() {
    let ToolSpec::Function(ResponsesApiTool { output_schema, .. }) = create_list_agents_tool()
    else {
        panic!("list_agents should be a function tool");
    };

    assert_eq!(
        output_schema.expect("list_agents output schema")["properties"]["agents"]["items"]["properties"]
            ["agent_status"]["allOf"][0]["oneOf"][0]["enum"],
        json!([
            "pending_init",
            "running",
            "interrupted",
            "shutdown",
            "not_found"
        ])
    );
}
