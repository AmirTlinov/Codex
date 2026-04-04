use codex_tools::ToolSpec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaudeToolCallPayload {
    Function { arguments: String },
    Custom { input: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeToolCallMarkup {
    pub(crate) name: String,
    pub(crate) payload: ClaudeToolCallPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedClaudeAssistantMarkup {
    pub(crate) visible_text: String,
    pub(crate) tool_calls: Vec<ClaudeToolCallMarkup>,
}

pub(crate) fn parse_claude_tool_call_markup(
    text: &str,
    tools: &[ToolSpec],
) -> Result<Option<ParsedClaudeAssistantMarkup>, String> {
    if !text.contains("<tool_call>") {
        return Ok(None);
    }

    let mut remaining = text;
    let mut visible_text = String::new();
    let mut tool_calls = Vec::new();

    while let Some(start) = remaining.find("<tool_call>") {
        visible_text.push_str(&remaining[..start]);
        remaining = &remaining[start + "<tool_call>".len()..];

        let Some(end) = remaining.find("</tool_call>") else {
            return Err("Claude Code emitted an unterminated <tool_call> block".to_string());
        };
        let body = remaining[..end].trim();
        if body.is_empty() {
            return Err("Claude Code emitted an empty <tool_call> block".to_string());
        }

        tool_calls.push(parse_single_tool_call(body, tools)?);
        remaining = &remaining[end + "</tool_call>".len()..];
    }

    visible_text.push_str(remaining);
    if tool_calls.is_empty() {
        return Ok(None);
    }

    Ok(Some(ParsedClaudeAssistantMarkup {
        visible_text: visible_text.trim().to_string(),
        tool_calls,
    }))
}

fn parse_single_tool_call(body: &str, tools: &[ToolSpec]) -> Result<ClaudeToolCallMarkup, String> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|err| format!("failed to parse Claude <tool_call> JSON: {err}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "Claude <tool_call> JSON must be an object".to_string())?;
    let name = object
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "Claude <tool_call> JSON must include a non-empty `name`".to_string())?
        .to_string();

    let payload = match tool_payload_kind(tools, &name) {
        ToolPayloadKind::Custom => {
            let input = object
                .get("input")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    format!("Claude <tool_call> for `{name}` must include a string `input` field")
                })?
                .to_string();
            ClaudeToolCallPayload::Custom { input }
        }
        ToolPayloadKind::Function => {
            let arguments = object.get("arguments").ok_or_else(|| {
                format!("Claude <tool_call> for `{name}` must include an `arguments` field")
            })?;
            ClaudeToolCallPayload::Function {
                arguments: serde_json::to_string(arguments).map_err(|err| {
                    format!("failed to serialize Claude <tool_call> arguments for `{name}`: {err}")
                })?,
            }
        }
    };

    Ok(ClaudeToolCallMarkup { name, payload })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolPayloadKind {
    Function,
    Custom,
}

fn tool_payload_kind(tools: &[ToolSpec], name: &str) -> ToolPayloadKind {
    if name.starts_with("mcp__") {
        return ToolPayloadKind::Function;
    }

    match tools.iter().find(|tool| tool.name() == name) {
        Some(ToolSpec::Freeform(_)) => ToolPayloadKind::Custom,
        Some(ToolSpec::Function(_))
        | Some(ToolSpec::ToolSearch { .. })
        | Some(ToolSpec::LocalShell {})
        | Some(ToolSpec::ImageGeneration { .. })
        | Some(ToolSpec::WebSearch { .. })
        | None => ToolPayloadKind::Function,
    }
}

#[cfg(test)]
mod tests {
    use super::ClaudeToolCallMarkup;
    use super::ClaudeToolCallPayload;
    use super::ParsedClaudeAssistantMarkup;
    use super::parse_claude_tool_call_markup;
    use codex_tools::FreeformTool;
    use codex_tools::FreeformToolFormat;
    use codex_tools::JsonSchema;
    use codex_tools::ResponsesApiTool;
    use codex_tools::ToolSpec;
    use pretty_assertions::assert_eq;

    fn function_tool(name: &str) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: name.to_string(),
            description: "desc".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::Object {
                properties: std::collections::BTreeMap::new(),
                required: None,
                additional_properties: Some(false.into()),
            },
            output_schema: None,
        })
    }

    fn custom_tool(name: &str) -> ToolSpec {
        ToolSpec::Freeform(FreeformTool {
            name: name.to_string(),
            description: "desc".to_string(),
            format: FreeformToolFormat {
                r#type: "text".to_string(),
                syntax: "raw".to_string(),
                definition: "raw".to_string(),
            },
        })
    }

    #[test]
    fn parse_tool_call_markup_returns_visible_text_and_function_call() {
        let parsed = parse_claude_tool_call_markup(
            "Launching worker.\n\n<tool_call>\n{\"name\":\"spawn_agent\",\"arguments\":{\"model\":\"gpt-5.4\",\"message\":\"ping\"}}\n</tool_call>",
            &[function_tool("spawn_agent")],
        )
        .expect("tool_call markup should parse");

        assert_eq!(
            parsed,
            Some(ParsedClaudeAssistantMarkup {
                visible_text: "Launching worker.".to_string(),
                tool_calls: vec![ClaudeToolCallMarkup {
                    name: "spawn_agent".to_string(),
                    payload: ClaudeToolCallPayload::Function {
                        arguments: "{\"message\":\"ping\",\"model\":\"gpt-5.4\"}".to_string(),
                    },
                }],
            })
        );
    }

    #[test]
    fn parse_tool_call_markup_uses_input_for_custom_tools() {
        let parsed = parse_claude_tool_call_markup(
            "<tool_call>{\"name\":\"apply_patch\",\"input\":\"*** Begin Patch\"}</tool_call>",
            &[custom_tool("apply_patch")],
        )
        .expect("custom tool_call markup should parse");

        assert_eq!(
            parsed,
            Some(ParsedClaudeAssistantMarkup {
                visible_text: String::new(),
                tool_calls: vec![ClaudeToolCallMarkup {
                    name: "apply_patch".to_string(),
                    payload: ClaudeToolCallPayload::Custom {
                        input: "*** Begin Patch".to_string(),
                    },
                }],
            })
        );
    }

    #[test]
    fn parse_tool_call_markup_rejects_missing_arguments_for_function_tools() {
        let err = parse_claude_tool_call_markup(
            "<tool_call>{\"name\":\"spawn_agent\"}</tool_call>",
            &[function_tool("spawn_agent")],
        )
        .expect_err("missing arguments should fail");

        assert_eq!(
            err,
            "Claude <tool_call> for `spawn_agent` must include an `arguments` field"
        );
    }

    #[test]
    fn parse_tool_call_markup_treats_namespaced_mcp_tools_as_function_calls() {
        let parsed = parse_claude_tool_call_markup(
            "<tool_call>{\"name\":\"mcp__codex__codex-shell\",\"arguments\":{\"command\":\"printf hi\"}}</tool_call>",
            &[],
        )
        .expect("mcp tool_call markup should parse");

        assert_eq!(
            parsed,
            Some(ParsedClaudeAssistantMarkup {
                visible_text: String::new(),
                tool_calls: vec![ClaudeToolCallMarkup {
                    name: "mcp__codex__codex-shell".to_string(),
                    payload: ClaudeToolCallPayload::Function {
                        arguments: "{\"command\":\"printf hi\"}".to_string(),
                    },
                }],
            })
        );
    }
}
