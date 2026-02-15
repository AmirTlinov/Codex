use codex_protocol::models::FunctionCallOutputBody;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

use crate::agent::AgentRole;
use crate::apply_patch;
use crate::apply_patch::InternalApplyPatchInvocation;
use crate::apply_patch::convert_apply_patch_to_protocol;
use crate::client_common::tools::FreeformTool;
use crate::client_common::tools::FreeformToolFormat;
use crate::client_common::tools::ResponsesApiTool;
use crate::client_common::tools::ToolSpec;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::function_tool::FunctionCallError;
use crate::protocol::AskForApproval;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::events::ToolEmitter;
use crate::tools::events::ToolEventCtx;
use crate::tools::handlers::parse_arguments;
use crate::tools::orchestrator::ToolOrchestrator;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use crate::tools::runtimes::apply_patch::ApplyPatchRequest;
use crate::tools::runtimes::apply_patch::ApplyPatchRuntime;
use crate::tools::sandboxing::ToolCtx;
use crate::tools::spec::ApplyPatchToolArgs;
use crate::tools::spec::JsonSchema;
use async_trait::async_trait;
use codex_apply_patch::ApplyPatchAction;
use codex_apply_patch::ApplyPatchFileChange;
use codex_protocol::config_types::ModeKind;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::path::Component;
use std::sync::atomic::Ordering;

pub struct ApplyPatchHandler;

const APPLY_PATCH_LARK_GRAMMAR: &str = include_str!("tool_apply_patch.lark");

fn file_paths_for_action(action: &ApplyPatchAction) -> Vec<AbsolutePathBuf> {
    let mut keys = Vec::new();
    let cwd = action.cwd.as_path();

    for (path, change) in action.changes() {
        if let Some(key) = to_abs_path(cwd, path) {
            keys.push(key);
        }

        if let ApplyPatchFileChange::Update { move_path, .. } = change
            && let Some(dest) = move_path
            && let Some(key) = to_abs_path(cwd, dest)
        {
            keys.push(key);
        }
    }

    keys
}

fn to_abs_path(cwd: &Path, path: &Path) -> Option<AbsolutePathBuf> {
    AbsolutePathBuf::resolve_path_against_base(path, cwd).ok()
}

fn plan_patch_paths(action: &ApplyPatchAction) -> Result<Vec<AbsolutePathBuf>, String> {
    let mut paths = Vec::new();
    let cwd = action.cwd.as_path();
    for (path, change) in action.changes() {
        let source = to_abs_path(cwd, path).ok_or_else(|| {
            format!(
                "Plan agent can only patch resolved paths; failed to resolve `{}`",
                path.display()
            )
        })?;
        paths.push(source);

        if let ApplyPatchFileChange::Update { move_path, .. } = change
            && let Some(dest) = move_path
        {
            let destination = to_abs_path(cwd, dest).ok_or_else(|| {
                format!(
                    "Plan agent can only patch resolved paths; failed to resolve move target `{}`",
                    dest.display()
                )
            })?;
            paths.push(destination);
        }
    }
    Ok(paths)
}

fn is_allowed_plan_file_path(relative_path: &Path) -> bool {
    let components: Vec<Component<'_>> = relative_path.components().collect();
    if components.len() != 3 {
        return false;
    }

    let [repo_component, plan_component, file_component] = components.as_slice() else {
        return false;
    };
    if !matches!(repo_component, Component::Normal(_))
        || !matches!(plan_component, Component::Normal(_))
    {
        return false;
    }

    let Component::Normal(file_name) = file_component else {
        return false;
    };
    let Some(file_name) = file_name.to_str() else {
        return false;
    };
    file_name == "PLAN.md" || (file_name.starts_with("slice-") && file_name.ends_with(".md"))
}

fn validate_plan_patch_targets(
    action: &ApplyPatchAction,
    codex_home: &Path,
    existing_plan_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    let plans_root = codex_home.join("plans");
    let mut plan_dir: Option<PathBuf> = None;

    for path in plan_patch_paths(action)? {
        let absolute_path = path.as_path();
        let relative = absolute_path.strip_prefix(&plans_root).map_err(|_| {
            format!(
                "Plan agent can only write under `{}`. Move `{}` into `~/.codex/plans/<repository>_<session>/<plan_name>/`.",
                plans_root.display(),
                absolute_path.display()
            )
        })?;
        if !is_allowed_plan_file_path(relative) {
            return Err(format!(
                "Plan agent can only write `PLAN.md` and `slice-*.md` files under `~/.codex/plans/<repository>_<session>/<plan_name>/`; rejected `{}`.",
                absolute_path.display()
            ));
        }

        let file_dir = absolute_path.parent().ok_or_else(|| {
            format!(
                "Plan patch path `{}` must be within a plan directory.",
                absolute_path.display()
            )
        })?;
        if let Some(expected) = existing_plan_dir
            && file_dir != expected
        {
            return Err(format!(
                "Plan patch targets must use existing plan directory `{}`. Got `{}`.",
                expected.display(),
                file_dir.display()
            ));
        }

        match plan_dir.as_deref() {
            Some(current) if current != file_dir => {
                return Err(format!(
                    "Plan patch contains multiple plan directories in one patch: `{}` and `{}`.",
                    current.display(),
                    file_dir.display()
                ));
            }
            None => plan_dir = Some(file_dir.to_path_buf()),
            Some(_) => {}
        }
    }

    plan_dir.ok_or_else(|| {
        "Plan agent can only patch one plan directory and at least one file must be provided."
            .to_string()
    })
}

fn validator_allowed_patch_inputs(user_messages: &[String]) -> Vec<String> {
    let mut allowed = BTreeSet::new();
    for message in user_messages.iter().rev().take(3) {
        let trimmed_message = message.trim();
        if !trimmed_message.is_empty() {
            allowed.insert(trimmed_message.to_string());
        }

        let mut tail = message.as_str();
        while let Some(start) = tail.find("```") {
            let after_open = &tail[start + 3..];
            let Some(end) = after_open.find("```") else {
                break;
            };
            let mut candidate = after_open[..end].trim();
            if let Some((first_line, rest)) = candidate.split_once('\n')
                && matches!(first_line.trim(), "diff" | "patch" | "apply_patch")
            {
                candidate = rest.trim();
            }
            if !candidate.is_empty() {
                allowed.insert(candidate.to_string());
            }
            tail = &after_open[end + 3..];
        }
    }
    allowed.into_iter().collect()
}

fn validator_patch_is_verbatim_allowed(patch_input: &str, user_messages: &[String]) -> bool {
    let normalized_patch = patch_input.trim();
    if normalized_patch.is_empty() {
        return false;
    }

    validator_allowed_patch_inputs(user_messages)
        .into_iter()
        .any(|candidate| candidate.trim() == normalized_patch)
}

fn missing_default_pipeline_stages(turn: &TurnContext) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if !turn.scout_context_ready.load(Ordering::Acquire) {
        missing.push("scout");
    }
    if !turn.context_validated.load(Ordering::Acquire) {
        missing.push("context_validator");
    }
    if !turn.builder_spawned.load(Ordering::Acquire) {
        missing.push("builder");
    }
    if !turn.validator_spawned.load(Ordering::Acquire) {
        missing.push("validator (or post_builder_validator)");
    }
    missing
}

async fn enforce_apply_patch_guards(
    session: &Session,
    turn: &TurnContext,
    patch_input: &str,
    changes: &ApplyPatchAction,
) -> Result<(), FunctionCallError> {
    enforce_apply_patch_pre_parse_guards(session, turn, patch_input).await?;

    if turn.tools_config.agent_role == AgentRole::Plan {
        let mut active_plan_dir = session.active_plan_dir.lock().await;
        let existing_plan_dir = active_plan_dir.as_ref().cloned();
        let validated_plan_dir = validate_plan_patch_targets(
            changes,
            &turn.config.codex_home,
            existing_plan_dir.as_deref(),
        )
        .map_err(FunctionCallError::RespondToModel)?;
        if existing_plan_dir.as_ref() != Some(&validated_plan_dir) {
            *active_plan_dir = Some(validated_plan_dir);
        }
    }

    Ok(())
}

async fn enforce_apply_patch_pre_parse_guards(
    session: &Session,
    turn: &TurnContext,
    patch_input: &str,
) -> Result<(), FunctionCallError> {
    if turn.tools_config.agent_role == AgentRole::Default
        && turn.tools_config.collaboration_mode != ModeKind::Plan
        && turn.tools_config.collab_tools
        && matches!(turn.approval_policy, AskForApproval::Never)
    {
        let missing = missing_default_pipeline_stages(turn);
        if !missing.is_empty() {
            return Err(FunctionCallError::RespondToModel(format!(
                "apply_patch is locked in Default role until the pipeline is complete: scout -> context_validator -> builder -> validator. Missing: {}.",
                missing.join(", ")
            )));
        }
    }

    if matches!(
        turn.tools_config.agent_role,
        AgentRole::Validator | AgentRole::PostBuilderValidator
    ) {
        let history = session.clone_history().await;
        let user_messages = crate::compact::collect_user_messages(history.raw_items());
        if !validator_patch_is_verbatim_allowed(patch_input, &user_messages) {
            return Err(FunctionCallError::RespondToModel(
                "Validator can only apply a Builder patch verbatim. If the patch needs changes, reject with a detailed, file-specific explanation instead."
                    .to_string(),
            ));
        }
    }

    Ok(())
}

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

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
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
                let args: ApplyPatchToolArgs = parse_arguments(&arguments)?;
                args.input
            }
            ToolPayload::Custom { input } => input,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "apply_patch handler received unsupported payload".to_string(),
                ));
            }
        };

        enforce_apply_patch_pre_parse_guards(session.as_ref(), turn.as_ref(), &patch_input).await?;

        // Re-parse and verify the patch so we can compute changes and approval.
        // Avoid building temporary ExecParams/command vectors; derive directly from inputs.
        let cwd = turn.cwd.clone();
        let command = vec!["apply_patch".to_string(), patch_input.clone()];
        match codex_apply_patch::maybe_parse_apply_patch_verified(&command, &cwd) {
            codex_apply_patch::MaybeApplyPatchVerified::Body(changes) => {
                enforce_apply_patch_guards(session.as_ref(), turn.as_ref(), &patch_input, &changes)
                    .await?;
                match apply_patch::apply_patch(turn.as_ref(), changes).await {
                    InternalApplyPatchInvocation::Output(item) => {
                        let content = item?;
                        Ok(ToolOutput::Function {
                            body: FunctionCallOutputBody::Text(content),
                            success: Some(true),
                        })
                    }
                    InternalApplyPatchInvocation::DelegateToExec(apply) => {
                        let changes = convert_apply_patch_to_protocol(&apply.action);
                        let file_paths = file_paths_for_action(&apply.action);
                        let emitter =
                            ToolEmitter::apply_patch(changes.clone(), apply.auto_approved);
                        let event_ctx = ToolEventCtx::new(
                            session.as_ref(),
                            turn.as_ref(),
                            &call_id,
                            Some(&tracker),
                        );
                        emitter.begin(event_ctx).await;

                        let req = ApplyPatchRequest {
                            action: apply.action,
                            file_paths,
                            changes,
                            exec_approval_requirement: apply.exec_approval_requirement,
                            timeout_ms: None,
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
                            body: FunctionCallOutputBody::Text(content),
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn intercept_apply_patch(
    command: &[String],
    cwd: &Path,
    timeout_ms: Option<u64>,
    session: &Session,
    turn: &TurnContext,
    tracker: Option<&SharedTurnDiffTracker>,
    call_id: &str,
    tool_name: &str,
) -> Result<Option<ToolOutput>, FunctionCallError> {
    let patch_input = command.get(1).cloned().unwrap_or_else(|| command.join(" "));
    if command
        .iter()
        .any(|segment| segment == "apply_patch" || segment.contains("apply_patch"))
    {
        enforce_apply_patch_pre_parse_guards(session, turn, &patch_input).await?;
    }

    match codex_apply_patch::maybe_parse_apply_patch_verified(command, cwd) {
        codex_apply_patch::MaybeApplyPatchVerified::Body(changes) => {
            session
                .record_model_warning(
                    format!("apply_patch was requested via {tool_name}. Use the apply_patch tool instead of exec_command."),
                    turn,
                )
                .await;
            enforce_apply_patch_guards(session, turn, &patch_input, &changes).await?;
            match apply_patch::apply_patch(turn, changes).await {
                InternalApplyPatchInvocation::Output(item) => {
                    let content = item?;
                    Ok(Some(ToolOutput::Function {
                        body: FunctionCallOutputBody::Text(content),
                        success: Some(true),
                    }))
                }
                InternalApplyPatchInvocation::DelegateToExec(apply) => {
                    let changes = convert_apply_patch_to_protocol(&apply.action);
                    let approval_keys = file_paths_for_action(&apply.action);
                    let emitter = ToolEmitter::apply_patch(changes.clone(), apply.auto_approved);
                    let event_ctx =
                        ToolEventCtx::new(session, turn, call_id, tracker.as_ref().copied());
                    emitter.begin(event_ctx).await;

                    let req = ApplyPatchRequest {
                        action: apply.action,
                        file_paths: approval_keys,
                        changes,
                        exec_approval_requirement: apply.exec_approval_requirement,
                        timeout_ms,
                        codex_exe: turn.codex_linux_sandbox_exe.clone(),
                    };

                    let mut orchestrator = ToolOrchestrator::new();
                    let mut runtime = ApplyPatchRuntime::new();
                    let tool_ctx = ToolCtx {
                        session,
                        turn,
                        call_id: call_id.to_string(),
                        tool_name: tool_name.to_string(),
                    };
                    let out = orchestrator
                        .run(&mut runtime, &req, &tool_ctx, turn, turn.approval_policy)
                        .await;
                    let event_ctx =
                        ToolEventCtx::new(session, turn, call_id, tracker.as_ref().copied());
                    let content = emitter.finish(event_ctx, out).await?;
                    Ok(Some(ToolOutput::Function {
                        body: FunctionCallOutputBody::Text(content),
                        success: Some(true),
                    }))
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
            Ok(None)
        }
        codex_apply_patch::MaybeApplyPatchVerified::NotApplyPatch => Ok(None),
    }
}

/// Returns a custom tool that can be used to edit files. Well-suited for GPT-5 models
/// https://platform.openai.com/docs/guides/function-calling#custom-tools
pub(crate) fn create_apply_patch_freeform_tool() -> ToolSpec {
    ToolSpec::Freeform(FreeformTool {
        name: "apply_patch".to_string(),
        description: "Use the `apply_patch` tool to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.".to_string(),
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
        description: r#"Use the `apply_patch` tool to edit files.
Your patch language is a stripped‑down, file‑oriented diff format designed to be easy to parse and safe to apply. You can think of it as a high‑level envelope:

*** Begin Patch
[ one or more file sections ]
*** End Patch

Within that envelope, you get a sequence of file operations.
You MUST include a header to specify the action you are taking.
Each operation starts with one of three headers:

*** Add File: <path> - create a new file. Every following line is a + line (the initial contents).
*** Delete File: <path> - remove an existing file. Nothing follows.
*** Update File: <path> - patch an existing file in place (optionally with a rename).

May be immediately followed by *** Move to: <new path> if you want to rename the file.
Then one or more “hunks”, each introduced by @@ (optionally followed by a hunk header).
Within a hunk each line starts with:

For instructions on [context_before] and [context_after]:
- By default, show 3 lines of code immediately above and 3 lines immediately below each change. If a change is within 3 lines of a previous change, do NOT duplicate the first change’s [context_after] lines in the second change’s [context_before] lines.
- If 3 lines of context is insufficient to uniquely identify the snippet of code within the file, use the @@ operator to indicate the class or function to which the snippet belongs. For instance, we might have:
@@ class BaseClass
[3 lines of pre-context]
- [old_code]
+ [new_code]
[3 lines of post-context]

- If a code block is repeated so many times in a class or function such that even a single `@@` statement and 3 lines of context cannot uniquely identify the snippet of code, you can use multiple `@@` statements to jump to the right context. For instance:

@@ class BaseClass
@@ 	 def method():
[3 lines of pre-context]
- [old_code]
+ [new_code]
[3 lines of post-context]

The full grammar definition is below:
Patch := Begin { FileOp } End
Begin := "*** Begin Patch" NEWLINE
End := "*** End Patch" NEWLINE
FileOp := AddFile | DeleteFile | UpdateFile
AddFile := "*** Add File: " path NEWLINE { "+" line NEWLINE }
DeleteFile := "*** Delete File: " path NEWLINE
UpdateFile := "*** Update File: " path NEWLINE [ MoveTo ] { Hunk }
MoveTo := "*** Move to: " newPath NEWLINE
Hunk := "@@" [ header ] NEWLINE { HunkLine } [ "*** End of File" NEWLINE ]
HunkLine := (" " | "-" | "+") text NEWLINE

A full patch can combine several operations:

*** Begin Patch
*** Add File: hello.txt
+Hello world
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
-print("Hi")
+print("Hello, world!")
*** Delete File: obsolete.txt
*** End Patch

It is important to remember:

- You must include a header with your intended action (Add/Delete/Update)
- You must prefix new lines with `+` even when creating a new file
- File references can only be relative, NEVER ABSOLUTE.
"#
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["input".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::make_session_and_context;
    use crate::function_tool::FunctionCallError;
    use codex_apply_patch::MaybeApplyPatchVerified;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[test]
    fn approval_keys_include_move_destination() {
        let tmp = TempDir::new().expect("tmp");
        let cwd = tmp.path();
        std::fs::create_dir_all(cwd.join("old")).expect("create old dir");
        std::fs::create_dir_all(cwd.join("renamed/dir")).expect("create dest dir");
        std::fs::write(cwd.join("old/name.txt"), "old content\n").expect("write old file");
        let patch = r#"*** Begin Patch
*** Update File: old/name.txt
*** Move to: renamed/dir/name.txt
@@
-old content
+new content
*** End Patch"#;
        let argv = vec!["apply_patch".to_string(), patch.to_string()];
        let action = match codex_apply_patch::maybe_parse_apply_patch_verified(&argv, cwd) {
            MaybeApplyPatchVerified::Body(action) => action,
            other => panic!("expected patch body, got: {other:?}"),
        };

        let keys = file_paths_for_action(&action);
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn validator_patch_candidates_extract_fenced_blocks() {
        let message = r#"
Some explanation.
```diff
*** Begin Patch
*** Add File: test.txt
+hello
*** End Patch
```
"#;
        let candidates = validator_allowed_patch_inputs(&[message.to_string()]);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.contains("*** Begin Patch"))
        );
    }

    #[test]
    fn validator_patch_must_be_verbatim() {
        let patch = "*** Begin Patch\n*** Add File: test.txt\n+hello\n*** End Patch";
        let message = format!("Builder patch:\n```diff\n{patch}\n```");
        assert!(validator_patch_is_verbatim_allowed(
            patch,
            &[message.to_string()]
        ));
        assert!(!validator_patch_is_verbatim_allowed(
            "*** Begin Patch\n*** Add File: test.txt\n+hello world\n*** End Patch",
            &[message]
        ));
    }

    #[test]
    fn post_builder_validator_patch_must_be_verbatim() {
        let patch = "*** Begin Patch\n*** Add File: test.txt\n+hello\n*** End Patch";
        let message = format!("Builder patch:\n```diff\n{patch}\n```");
        assert!(validator_patch_is_verbatim_allowed(
            patch,
            &[message.to_string()]
        ));
        assert!(!validator_patch_is_verbatim_allowed(
            "*** Begin Patch\n*** Add File: test.txt\n+hello world\n*** End Patch",
            &[message]
        ));
    }

    #[test]
    fn plan_patch_accepts_plan_and_slice_files_only() {
        let tmp = TempDir::new().expect("tmp");
        let codex_home = tmp.path().join(".codex");
        let plans_root = codex_home.join("plans/repo_session/my-plan");
        std::fs::create_dir_all(&plans_root).expect("create plan dirs");

        let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+goal\n*** Add File: {}\n+slice\n*** End Patch",
            plans_root.join("PLAN.md").display(),
            plans_root.join("slice-1.md").display()
        );
        let command = vec!["apply_patch".to_string(), patch];
        let action = match codex_apply_patch::maybe_parse_apply_patch_verified(&command, tmp.path())
        {
            MaybeApplyPatchVerified::Body(action) => action,
            other => panic!("expected patch body, got: {other:?}"),
        };

        let result = validate_plan_patch_targets(&action, &codex_home, None)
            .expect("expected valid plan patch targets");
        assert_eq!(result, plans_root);
    }

    #[test]
    fn plan_patch_rejects_non_plan_file_targets() {
        let tmp = TempDir::new().expect("tmp");
        let codex_home = tmp.path().join(".codex");
        let plans_root = codex_home.join("plans/repo_session/my-plan");
        std::fs::create_dir_all(&plans_root).expect("create plan dirs");

        let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+notes\n*** End Patch",
            plans_root.join("notes.md").display()
        );
        let command = vec!["apply_patch".to_string(), patch];
        let action = match codex_apply_patch::maybe_parse_apply_patch_verified(&command, tmp.path())
        {
            MaybeApplyPatchVerified::Body(action) => action,
            other => panic!("expected patch body, got: {other:?}"),
        };

        let result = validate_plan_patch_targets(&action, &codex_home, None);
        assert!(matches!(result, Err(message) if message.contains("PLAN.md")));
    }

    #[test]
    fn plan_patch_rejects_targets_outside_codex_plans_root() {
        let tmp = TempDir::new().expect("tmp");
        let codex_home = tmp.path().join(".codex");
        let outside = tmp.path().join("outside.md");
        let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+outside\n*** End Patch",
            outside.display()
        );
        let command = vec!["apply_patch".to_string(), patch];
        let action = match codex_apply_patch::maybe_parse_apply_patch_verified(&command, tmp.path())
        {
            MaybeApplyPatchVerified::Body(action) => action,
            other => panic!("expected patch body, got: {other:?}"),
        };

        let result = validate_plan_patch_targets(&action, &codex_home, None);
        assert!(matches!(result, Err(message) if message.contains("/plans")));
    }

    #[test]
    fn plan_patch_rejects_changes_in_multiple_plan_dirs() {
        let tmp = TempDir::new().expect("tmp");
        let codex_home = tmp.path().join(".codex");
        let plans_root = codex_home.join("plans");
        let plan_a = plans_root.join("repo_session/plan-a");
        let plan_b = plans_root.join("repo_session/plan-b");
        std::fs::create_dir_all(&plan_a).expect("create plan dirs");
        std::fs::create_dir_all(&plan_b).expect("create plan dirs");

        let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+goal-a\n*** Add File: {}\n+slice-b\n*** End Patch",
            plan_a.join("PLAN.md").display(),
            plan_b.join("slice-1.md").display()
        );
        let command = vec!["apply_patch".to_string(), patch];
        let action = match codex_apply_patch::maybe_parse_apply_patch_verified(&command, tmp.path())
        {
            MaybeApplyPatchVerified::Body(action) => action,
            other => panic!("expected patch body, got: {other:?}"),
        };

        let result = validate_plan_patch_targets(&action, &codex_home, None);
        assert!(
            matches!(result.as_ref(), Err(message) if message.contains("multiple plan directories in one patch")),
            "unexpected result: {result:?}"
        );
    }

    #[test]
    fn plan_patch_rejects_changes_outside_existing_plan_dir() {
        let tmp = TempDir::new().expect("tmp");
        let codex_home = tmp.path().join(".codex");
        let plans_root = codex_home.join("plans");
        let plan_a = plans_root.join("repo_session/plan-a");
        let plan_b = plans_root.join("repo_session/plan-b");
        std::fs::create_dir_all(&plan_a).expect("create plan dirs");
        std::fs::create_dir_all(&plan_b).expect("create plan dirs");

        let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+goal\n*** End Patch",
            plan_a.join("PLAN.md").display()
        );
        let command = vec!["apply_patch".to_string(), patch];
        let action = match codex_apply_patch::maybe_parse_apply_patch_verified(&command, tmp.path())
        {
            MaybeApplyPatchVerified::Body(action) => action,
            other => panic!("expected patch body, got: {other:?}"),
        };

        let result = validate_plan_patch_targets(&action, &codex_home, Some(plan_b.as_path()));
        assert!(
            result
                .as_ref()
                .is_err_and(|message| message
                    .contains("Plan patch targets must use existing plan directory")),
            "unexpected result: {result:?}"
        );
    }

    #[tokio::test]
    async fn intercept_apply_patch_enforces_default_pipeline_guard() {
        let (session, mut turn) = make_session_and_context().await;
        turn.tools_config.agent_role = AgentRole::Default;
        turn.tools_config.collaboration_mode = ModeKind::Default;
        turn.tools_config.collab_tools = true;
        turn.approval_policy = AskForApproval::Never;

        let command = vec![
            "apply_patch".to_string(),
            "*** Begin Patch\n*** Add File: bypass.txt\n+blocked\n*** End Patch".to_string(),
        ];
        let result = intercept_apply_patch(
            &command,
            turn.cwd.as_path(),
            None,
            &session,
            &turn,
            None,
            "call-1",
            "shell",
        )
        .await;

        match result {
            Err(FunctionCallError::RespondToModel(message)) => {
                assert!(message.contains("scout -> context_validator -> builder -> validator"));
                assert!(message.contains("scout"));
            }
            _ => panic!("expected pipeline guard failure"),
        }
    }

    #[tokio::test]
    async fn intercept_apply_patch_enforces_default_pipeline_guard_before_parse() {
        let (session, mut turn) = make_session_and_context().await;
        turn.tools_config.agent_role = AgentRole::Default;
        turn.tools_config.collaboration_mode = ModeKind::Default;
        turn.tools_config.collab_tools = true;
        turn.approval_policy = AskForApproval::Never;

        let command = vec!["apply_patch".to_string(), "*** Begin Patch".to_string()];
        let result = intercept_apply_patch(
            &command,
            turn.cwd.as_path(),
            None,
            &session,
            &turn,
            None,
            "call-1",
            "shell",
        )
        .await;

        match result {
            Err(FunctionCallError::RespondToModel(message)) => {
                assert!(message.contains("scout -> context_validator -> builder -> validator"));
                assert!(message.contains("scout"));
            }
            _ => panic!("expected pipeline guard failure"),
        }
    }

    #[tokio::test]
    async fn intercept_apply_patch_enforces_plan_target_validation() {
        let tmp = TempDir::new().expect("tmp");
        let codex_home = tmp.path().join(".codex");
        std::fs::create_dir_all(codex_home.join("plans")).expect("create plans root");

        let (session, mut turn) = make_session_and_context().await;
        turn.tools_config.agent_role = AgentRole::Plan;
        turn.tools_config.collaboration_mode = ModeKind::Plan;
        let mut config = (*turn.config).clone();
        config.codex_home = codex_home.clone();
        turn.config = std::sync::Arc::new(config);

        let outside = tmp.path().join("outside.md");
        let command = vec![
            "apply_patch".to_string(),
            format!(
                "*** Begin Patch\n*** Add File: {}\n+outside\n*** End Patch",
                outside.display()
            ),
        ];
        let result = intercept_apply_patch(
            &command,
            turn.cwd.as_path(),
            None,
            &session,
            &turn,
            None,
            "call-1",
            "shell",
        )
        .await;

        match result {
            Err(FunctionCallError::RespondToModel(message)) => {
                assert!(message.contains("/plans"));
            }
            _ => panic!("expected plan path validation failure"),
        }
    }
}
