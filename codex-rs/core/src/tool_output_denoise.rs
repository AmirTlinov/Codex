use crate::memory::Block;
use crate::memory::BlockKind;
use crate::memory::BlockPriority;
use crate::memory::BlockStatus;
use crate::memory::BlockStore;
use crate::memory::SourceKind;
use crate::memory::SourceRef;
use crate::truncate::TruncationPolicy;
use crate::truncate::approx_token_count;
use crate::truncate::truncate_text;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use std::borrow::Cow;
use std::collections::HashMap;
use std::io;

const MAX_PROMPT_OUTPUT_TOKENS: usize = 512;
const DIGEST_SNIPPET_TOKENS: usize = 192;

pub(crate) fn denoise_for_prompt(items: &mut [ResponseItem]) {
    let tool_names = collect_tool_names(items.iter());
    for item in items.iter_mut() {
        match item {
            ResponseItem::FunctionCallOutput { call_id, output } => {
                if contains_images(output) {
                    continue;
                }
                let text = function_output_text(output);
                if !should_denoise_text(text.as_ref()) {
                    continue;
                }
                let tool_name = tool_names.get(call_id).map(String::as_str);
                let digest = build_digest(tool_name, call_id, text.as_ref());
                output.content = digest;
                output.content_items = None;
            }
            ResponseItem::CustomToolCallOutput { call_id, output } => {
                if !should_denoise_text(output.as_str()) {
                    continue;
                }
                let tool_name = tool_names.get(call_id).map(String::as_str);
                let digest = build_digest(tool_name, call_id, output.as_str());
                *output = digest;
            }
            _ => {}
        }
    }
}

pub(crate) async fn archive_outputs(
    store: &mut BlockStore,
    turn_id: &str,
    items: &[ResponseItem],
) -> io::Result<()> {
    let tool_names = collect_tool_names(items.iter());
    for item in items {
        match item {
            ResponseItem::FunctionCallOutput { call_id, output } => {
                if contains_images(output) {
                    continue;
                }
                let text = function_output_text(output);
                if !should_denoise_text(text.as_ref()) {
                    continue;
                }
                let tool_name = tool_names.get(call_id).map(String::as_str);
                let block = build_tool_slice_block(tool_name, call_id, turn_id, text.as_ref());
                store.upsert(block).await?;
            }
            ResponseItem::CustomToolCallOutput { call_id, output } => {
                if !should_denoise_text(output.as_str()) {
                    continue;
                }
                let tool_name = tool_names.get(call_id).map(String::as_str);
                let block = build_tool_slice_block(tool_name, call_id, turn_id, output.as_str());
                store.upsert(block).await?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn should_denoise_text(text: &str) -> bool {
    approx_token_count(text) > MAX_PROMPT_OUTPUT_TOKENS
}

fn build_tool_slice_block(
    tool_name: Option<&str>,
    call_id: &str,
    turn_id: &str,
    output: &str,
) -> Block {
    let block_id = tool_slice_id(call_id);
    let display_tool = tool_name.unwrap_or("tool");
    let title = format!("{display_tool} output");

    let approx_tokens = approx_token_count(output);
    let bytes = output.len();
    let lines = output.lines().count();
    let digest = build_digest(tool_name, call_id, output);
    let label =
        format!("{block_id} ({display_tool}, ~{approx_tokens} tok, {lines} lines, {bytes} bytes)");

    let mut block = Block::new(block_id, BlockKind::ToolSlice, title);
    block.status = BlockStatus::Stashed;
    block.priority = BlockPriority::Low;
    block.tags = vec![
        "tool_slice".to_string(),
        "evidence".to_string(),
        display_tool.to_string(),
    ];
    block.body_full = Some(output.to_string());
    block.body_summary = Some(digest);
    block.body_label = Some(label);
    block.sources = vec![SourceRef {
        kind: SourceKind::ToolOutput,
        locator: format!("turn:{turn_id} call_id:{call_id} tool:{display_tool}"),
        fingerprint: None,
    }];
    block
}

fn build_digest(tool_name: Option<&str>, call_id: &str, output: &str) -> String {
    let approx_tokens = approx_token_count(output);
    let bytes = output.len();
    let lines = output.lines().count();
    let tool_prefix = tool_name
        .map(|name| format!("tool={name} "))
        .unwrap_or_default();
    let snippet = truncate_text(output, TruncationPolicy::Tokens(DIGEST_SNIPPET_TOKENS));
    let tool_slice = tool_slice_id(call_id);

    format!(
        "[tool output digest; {tool_prefix}call_id={call_id}; archived={tool_slice}; ~{approx_tokens} tok; {lines} lines; {bytes} bytes]\n{snippet}"
    )
}

fn tool_slice_id(call_id: &str) -> String {
    format!("tool_slice:{call_id}")
}

fn collect_tool_names<'a>(
    items: impl Iterator<Item = &'a ResponseItem>,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for item in items {
        match item {
            ResponseItem::FunctionCall { call_id, name, .. }
            | ResponseItem::CustomToolCall { call_id, name, .. } => {
                out.insert(call_id.clone(), name.clone());
            }
            _ => {}
        }
    }
    out
}

fn contains_images(output: &FunctionCallOutputPayload) -> bool {
    output.content_items.as_ref().is_some_and(|items| {
        items
            .iter()
            .any(|item| matches!(item, FunctionCallOutputContentItem::InputImage { .. }))
    })
}

fn function_output_text<'a>(output: &'a FunctionCallOutputPayload) -> Cow<'a, str> {
    if let Some(items) = output.content_items.as_ref() {
        let mut chunks = Vec::new();
        for item in items {
            if let FunctionCallOutputContentItem::InputText { text } = item {
                chunks.push(text.as_str());
            }
        }
        Cow::Owned(chunks.join("\n"))
    } else {
        Cow::Borrowed(output.content.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    fn big_text() -> String {
        std::iter::repeat_n("line\n", 8_000).collect()
    }

    #[test]
    fn denoise_for_prompt_replaces_large_outputs() {
        let output_text = big_text();
        let call_id = "call-1".to_string();
        let mut items = vec![
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                arguments: "{}".to_string(),
                call_id: call_id.clone(),
            },
            ResponseItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: output_text,
                    content_items: None,
                    success: Some(true),
                },
            },
        ];

        denoise_for_prompt(&mut items);

        let ResponseItem::FunctionCallOutput { output, .. } = &items[1] else {
            panic!("expected function_call_output");
        };
        assert_eq!(output.content_items, None);
        assert!(output.content.contains("tool output digest"));
        assert!(output.content.contains("tool=shell"));
        assert!(output.content.contains("archived=tool_slice:call-1"));
    }

    #[tokio::test]
    async fn archive_outputs_writes_tool_slice_blocks() -> io::Result<()> {
        let temp = TempDir::new()?;
        let root = AbsolutePathBuf::try_from(temp.path().join("memory"))?;
        let cwd = temp.path().join("project");
        tokio::fs::create_dir_all(&cwd).await?;

        let mut store = BlockStore::open(&root, &cwd).await?;
        let call_id = "call-1".to_string();
        let output_text = big_text();
        let items = vec![
            ResponseItem::CustomToolCall {
                id: None,
                status: None,
                call_id: call_id.clone(),
                name: "web.run".to_string(),
                input: "{}".to_string(),
            },
            ResponseItem::CustomToolCallOutput {
                call_id: call_id.clone(),
                output: output_text.clone(),
            },
        ];

        archive_outputs(&mut store, "turn-1", &items).await?;

        let block_id = tool_slice_id(call_id.as_str());
        let block = store.get(block_id.as_str()).expect("tool slice stored");
        assert_eq!(block.kind, BlockKind::ToolSlice);
        assert_eq!(block.status, BlockStatus::Stashed);
        assert_eq!(block.priority, BlockPriority::Low);
        assert_eq!(
            block.body_full.as_deref().map(str::len),
            Some(output_text.len())
        );
        assert!(block.body_label.as_deref().unwrap().contains("tool_slice:"));
        assert!(block.tags.iter().any(|tag| tag == "evidence"));
        assert!(block.tags.iter().any(|tag| tag == "web.run"));
        Ok(())
    }

    #[test]
    fn denoise_skips_outputs_with_images() {
        let output_text = big_text();
        let call_id = "call-1".to_string();
        let mut items = vec![ResponseItem::FunctionCallOutput {
            call_id,
            output: FunctionCallOutputPayload {
                content: output_text.clone(),
                content_items: Some(vec![FunctionCallOutputContentItem::InputImage {
                    image_url: "data:...".to_string(),
                }]),
                success: Some(true),
            },
        }];

        denoise_for_prompt(&mut items);

        let ResponseItem::FunctionCallOutput { output, .. } = &items[0] else {
            panic!("expected function_call_output");
        };
        assert_eq!(output.content, output_text);
    }
}
