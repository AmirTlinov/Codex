use std::collections::BTreeMap;

use crate::apply_patch;
use crate::apply_patch::InternalApplyPatchInvocation;
use crate::apply_patch::convert_apply_patch_to_protocol;
use crate::client_common::tools::FreeformTool;
use crate::client_common::tools::FreeformToolFormat;
use crate::client_common::tools::ResponsesApiTool;
use crate::client_common::tools::ToolSpec;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::events::ToolEmitter;
use crate::tools::events::ToolEventCtx;
use crate::tools::orchestrator::ToolOrchestrator;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use crate::tools::runtimes::apply_patch::ApplyPatchRequest;
use crate::tools::runtimes::apply_patch::ApplyPatchRuntime;
use crate::tools::sandboxing::ToolCtx;
use crate::tools::spec::ApplyPatchToolArgs;
use crate::tools::spec::JsonSchema;
use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;

pub struct ApplyPatchHandler;

const APPLY_PATCH_LARK_GRAMMAR: &str = include_str!("tool_apply_patch.lark");

#[async_trait]
impl ToolHandler for ApplyPatchHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(
            payload,
            ToolPayload::Function { .. } | ToolPayload::Custom { .. }
        )
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            tracker,
            call_id,
            tool_name,
            payload,
        } = invocation;

        let patch_input = match payload {
            ToolPayload::Function { arguments } => {
                let args: ApplyPatchToolArgs = serde_json::from_str(&arguments).map_err(|e| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to parse function arguments: {e:?}"
                    ))
                })?;
                args.input
            }
            ToolPayload::Custom { input } => input,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "apply_patch handler received unsupported payload".to_string(),
                ));
            }
        };

        // Re-parse and verify the patch so we can compute changes and approval.
        // Avoid building temporary ExecParams/command vectors; derive directly from inputs.
        let cwd = turn.cwd.clone();
        let command = vec!["apply_patch".to_string(), patch_input.clone()];
        match codex_apply_patch::maybe_parse_apply_patch_verified(&command, &cwd) {
            codex_apply_patch::MaybeApplyPatchVerified::Body(changes) => {
                match apply_patch::apply_patch(session.as_ref(), turn.as_ref(), &call_id, changes)
                    .await
                {
                    InternalApplyPatchInvocation::Output(item) => {
                        let content = item?;
                        Ok(ToolOutput::Function {
                            content,
                            content_items: None,
                            success: Some(true),
                        })
                    }
                    InternalApplyPatchInvocation::DelegateToExec(apply) => {
                        let emitter = ToolEmitter::apply_patch(
                            convert_apply_patch_to_protocol(&apply.action),
                            !apply.user_explicitly_approved_this_action,
                            None,
                        );
                        let event_ctx = ToolEventCtx::new(
                            session.as_ref(),
                            turn.as_ref(),
                            &call_id,
                            Some(&tracker),
                        );
                        emitter.begin(event_ctx).await;

                        let req = ApplyPatchRequest {
                            patch: apply.action.patch.clone(),
                            cwd: apply.action.cwd.clone(),
                            timeout_ms: None,
                            user_explicitly_approved: apply.user_explicitly_approved_this_action,
                            codex_exe: turn.codex_linux_sandbox_exe.clone(),
                        };

                        let mut orchestrator = ToolOrchestrator::new();
                        let mut runtime = ApplyPatchRuntime::new();
                        let tool_ctx = ToolCtx {
                            session: session.as_ref(),
                            turn: turn.as_ref(),
                            call_id: call_id.clone(),
                            tool_name: tool_name.to_string(),
                        };
                        let out = orchestrator
                            .run(&mut runtime, &req, &tool_ctx, &turn, turn.approval_policy)
                            .await;
                        let event_ctx = ToolEventCtx::new(
                            session.as_ref(),
                            turn.as_ref(),
                            &call_id,
                            Some(&tracker),
                        );
                        let content = emitter.finish(event_ctx, out).await?;
                        Ok(ToolOutput::Function {
                            content,
                            content_items: None,
                            success: Some(true),
                        })
                    }
                }
            }
            codex_apply_patch::MaybeApplyPatchVerified::CorrectnessError(parse_error) => {
                Err(FunctionCallError::RespondToModel(format!(
                    "apply_patch verification failed: {parse_error}"
                )))
            }
            codex_apply_patch::MaybeApplyPatchVerified::ShellParseError(error) => {
                tracing::trace!("Failed to parse apply_patch input, {error:?}");
                Err(FunctionCallError::RespondToModel(
                    "apply_patch handler received invalid patch input".to_string(),
                ))
            }
            codex_apply_patch::MaybeApplyPatchVerified::NotApplyPatch => {
                Err(FunctionCallError::RespondToModel(
                    "apply_patch handler received non-apply_patch input".to_string(),
                ))
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPatchToolType {
    Freeform,
    Function,
}

/// Returns a custom tool that can be used to edit files. Well-suited for GPT-5 models
/// https://platform.openai.com/docs/guides/function-calling#custom-tools
pub(crate) fn create_apply_patch_freeform_tool() -> ToolSpec {
    ToolSpec::Freeform(FreeformTool {
        name: "apply_patch".to_string(),
        description: r#"`apply_patch` drives Codex CLI file edits for GPT-5-class agents. Feed it a raw `*** Begin Patch` … `*** End Patch` block; the CLI auto-detects modes and never needs extra flags or config.

Patch envelope:
*** Begin Patch
[one or more operations]
*** End Patch

Operations (mix as needed):

File edits
*** Add File: <path> - create a new file (every line must start with `+`)
*** Delete File: <path> - remove a file
*** Update File: <path> - apply diff hunks to an existing file

Symbol edits
*** Insert Before Symbol: <path::SymbolPath> - insert lines before the declaration
*** Insert After Symbol: <path::SymbolPath> - insert lines after the declaration
*** Replace Symbol Body: <path::SymbolPath> - replace the body; provide only `+` lines

Optional headers
*** Move to: <new path> - place immediately after an Add/Update/Delete to rename the file

Example:

*** Begin Patch
*** Replace Symbol Body: src/lib.rs::greet
+{
+    println!("Hello, world!");
+}
*** End Patch

Failure handling:
- On errors the CLI prints diagnostics plus an amendment template you can edit and reapply.
- `apply_patch amend` reapplies only the corrected hunks from that template.
- `apply_patch dry-run` and `apply_patch explain` validate without touching the filesystem.

Output:
- Human-readable summary of operations, formatting, and checks.
- Trailing single-line JSON report (`{"schema":"apply_patch/v2",...}`) keeps machine consumers in sync.

Guidelines:
- Use workspace-relative paths only.
- Prefix new or inserted content with `+`.
- Prefer symbol directives over wide diff chunks whenever possible.
- No on-disk logs are written; everything you need is printed to stdout."#
            .to_string(),
        format: FreeformToolFormat {
            r#type: "grammar".to_string(),
            syntax: "lark".to_string(),
            definition: APPLY_PATCH_LARK_GRAMMAR.to_string(),
        },
    })
}

/// Returns a json tool that can be used to edit files. Should only be used with gpt-oss models
pub(crate) fn create_apply_patch_json_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "input".to_string(),
        JsonSchema::String {
            description: Some(r#"The entire contents of the apply_patch command"#.to_string()),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "apply_patch".to_string(),
        description: r#"`apply_patch` drives Codex CLI file edits for GPT-5-class agents. Feed it a raw `*** Begin Patch` … `*** End Patch` block; the CLI auto-detects modes and never needs extra flags or config.

Patch envelope:
*** Begin Patch
[one or more operations]
*** End Patch

Operations (mix as needed):

File edits
*** Add File: <path> - create a new file (every line must start with `+`)
*** Delete File: <path> - remove a file
*** Update File: <path> - apply diff hunks to an existing file

Symbol edits
*** Insert Before Symbol: <path::SymbolPath> - insert lines before the declaration
*** Insert After Symbol: <path::SymbolPath> - insert lines after the declaration
*** Replace Symbol Body: <path::SymbolPath> - replace the body; provide only `+` lines

Optional headers
*** Move to: <new path> - place immediately after an Add/Update/Delete to rename the file

Example:

*** Begin Patch
*** Replace Symbol Body: src/lib.rs::greet
+{
+    println!("Hello, world!");
+}
*** End Patch

Failure handling:
- On errors the CLI prints diagnostics plus an amendment template you can edit and reapply.
- `apply_patch amend` reapplies only the corrected hunks from that template.
- `apply_patch dry-run` and `apply_patch explain` validate without touching the filesystem.

Output:
- Human-readable summary of operations, formatting, and checks.
- Trailing single-line JSON report (`{"schema":"apply_patch/v2",...}`) keeps machine consumers in sync.

Guidelines:
- Use workspace-relative paths only.
- Prefix new or inserted content with `+`.
- Prefer symbol directives over wide diff chunks whenever possible.
- No on-disk logs are written; everything you need is printed to stdout."#
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["input".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}
