use crate::codex::Session;
use crate::codex::TurnContext;
use crate::function_tool::FunctionCallError;
use crate::protocol::FileChange;
use crate::protocol::ReviewDecision;
use crate::safety::SafetyCheck;
use crate::safety::assess_patch_safety;
use crate::sandboxing::assessment::SandboxAssessmentRequest;
use codex_apply_patch::ApplyPatchAction;
use codex_apply_patch::ApplyPatchFileChange;
use codex_protocol::protocol::SandboxCommandAssessment;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

pub const CODEX_APPLY_PATCH_ARG1: &str = "--codex-run-as-apply-patch";

#[derive(Debug)]
pub(crate) enum InternalApplyPatchInvocation {
    /// The `apply_patch` call was handled programmatically, without any sort
    /// of sandbox, because the user explicitly approved it. This is the
    /// result to use with the `shell` function call that contained `apply_patch`.
    Output(Result<String, FunctionCallError>),

    /// The `apply_patch` call was approved, either automatically because it
    /// appears that it should be allowed based on the user's sandbox policy
    /// *or* because the user explicitly approved it. In either case, we use
    /// exec with [`CODEX_APPLY_PATCH_ARG1`] to realize the `apply_patch` call,
    /// but [`ApplyPatchExec::auto_approved`] is used to determine the sandbox
    /// used with the `exec()`.
    DelegateToExec(ApplyPatchExec),
}

#[derive(Debug)]
pub(crate) struct ApplyPatchExec {
    pub(crate) action: ApplyPatchAction,
    pub(crate) user_explicitly_approved_this_action: bool,
    pub(crate) risk: Option<SandboxCommandAssessment>,
}

pub(crate) async fn apply_patch(
    sess: &Session,
    turn_context: &TurnContext,
    tool_name: &str,
    call_id: &str,
    action: ApplyPatchAction,
) -> InternalApplyPatchInvocation {
    match assess_patch_safety(
        &action,
        turn_context.approval_policy,
        &turn_context.sandbox_policy,
        &turn_context.cwd,
    ) {
        SafetyCheck::AutoApprove {
            user_explicitly_approved,
            ..
        } => {
            let risk = maybe_assess_apply_patch(sess, turn_context, call_id, &action).await;
            InternalApplyPatchInvocation::DelegateToExec(ApplyPatchExec {
                action,
                user_explicitly_approved_this_action: user_explicitly_approved,
                risk,
            })
        }
        SafetyCheck::AskUser => {
            // Compute a readable summary of path changes to include in the
            // approval request so the user can make an informed decision.
            //
            // Note that it might be worth expanding this approval request to
            // give the user the option to expand the set of writable roots so
            // that similar patches can be auto-approved in the future during
            // this session.
            let rx_approve = sess
                .request_patch_approval(
                    turn_context.sub_id.clone(),
                    call_id.to_owned(),
                    tool_name.to_string(),
                    turn_context.client.get_otel_event_manager(),
                    &action,
                    None,
                    None,
                )
                .await;
            match rx_approve.await.unwrap_or_default() {
                ReviewDecision::Approved | ReviewDecision::ApprovedForSession => {
                    let risk = maybe_assess_apply_patch(sess, turn_context, call_id, &action).await;
                    InternalApplyPatchInvocation::DelegateToExec(ApplyPatchExec {
                        action,
                        user_explicitly_approved_this_action: true,
                        risk,
                    })
                }
                ReviewDecision::Denied | ReviewDecision::Abort => {
                    InternalApplyPatchInvocation::Output(Err(FunctionCallError::RespondToModel(
                        "patch rejected by user".to_string(),
                    )))
                }
            }
        }
        SafetyCheck::Reject { reason } => InternalApplyPatchInvocation::Output(Err(
            FunctionCallError::RespondToModel(format!("patch rejected: {reason}")),
        )),
    }
}

pub(crate) fn convert_apply_patch_to_protocol(
    action: &ApplyPatchAction,
) -> HashMap<PathBuf, FileChange> {
    let changes = action.changes();
    let mut result = HashMap::with_capacity(changes.len());
    for (path, change) in changes {
        let protocol_change = match change {
            ApplyPatchFileChange::Add { content } => FileChange::Add {
                content: content.clone(),
            },
            ApplyPatchFileChange::Delete { content } => FileChange::Delete {
                content: content.clone(),
            },
            ApplyPatchFileChange::Update {
                unified_diff,
                move_path,
                new_content: _new_content,
            } => FileChange::Update {
                unified_diff: unified_diff.clone(),
                move_path: move_path.clone(),
            },
        };
        result.insert(path.clone(), protocol_change);
    }
    result
}

fn sanitized_apply_patch_command(action: &ApplyPatchAction) -> Vec<String> {
    if let Some(command) = &action.command {
        return command.iter().map(|arg| sanitize_arg(arg, 512)).collect();
    }

    const MAX_SUMMARY_ENTRIES: usize = 8;
    let mut summary: Vec<_> = action.changes().iter().collect();
    summary.sort_by(|(path_a, _), (path_b, _)| path_a.cmp(path_b));

    let mut command = vec!["apply_patch".to_string()];
    let total = summary.len();
    for (idx, (path, change)) in summary.into_iter().enumerate() {
        if idx >= MAX_SUMMARY_ENTRIES {
            command.push(format!("+{} more changes", total - MAX_SUMMARY_ENTRIES));
            break;
        }
        let kind = match change {
            ApplyPatchFileChange::Add { .. } => "add",
            ApplyPatchFileChange::Delete { .. } => "delete",
            ApplyPatchFileChange::Update { .. } => "update",
        };
        command.push(format!(
            "{kind}:{}",
            relative_path_display(path.as_path(), action.cwd.as_path())
        ));
    }
    if total == 0 {
        command.push("no_changes".to_string());
    }
    command.push(format!("total_changes={total}"));
    command
}

fn sanitize_arg(arg: &str, max_len: usize) -> String {
    if arg.len() <= max_len {
        arg.to_string()
    } else {
        format!("{}…", &arg[..max_len])
    }
}

fn relative_path_display(path: &Path, base: &Path) -> String {
    match path.strip_prefix(base) {
        Ok(rel) => rel.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

async fn maybe_assess_apply_patch(
    session: &Session,
    turn_context: &TurnContext,
    call_id: &str,
    action: &ApplyPatchAction,
) -> Option<SandboxCommandAssessment> {
    let client = &turn_context.client;
    let config = client.config_arc();
    if !config.experimental_sandbox_command_assessment {
        return None;
    }

    let request = SandboxAssessmentRequest {
        config,
        provider: client.get_provider(),
        auth_manager: client.get_auth_manager(),
        otel_event_manager: client.get_otel_event_manager(),
        conversation_id: session.conversation_id(),
        call_id: call_id.to_string(),
        command: sanitized_apply_patch_command(action),
        sandbox_policy: turn_context.sandbox_policy.clone(),
        cwd: action.cwd.clone(),
        failure_message: None,
    };

    let assessment = session.services.sandbox_assessment.assess(request).await?;
    session
        .record_risk_assessment(call_id.to_string(), assessment.clone())
        .await;
    Some(assessment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    use tempfile::tempdir;

    #[test]
    fn convert_apply_patch_maps_add_variant() {
        let tmp = tempdir().expect("tmp");
        let p = tmp.path().join("a.txt");
        // Create an action with a single Add change
        let action = ApplyPatchAction::new_add_for_test(&p, "hello".to_string());

        let got = convert_apply_patch_to_protocol(&action);

        assert_eq!(
            got.get(&p),
            Some(&FileChange::Add {
                content: "hello".to_string()
            })
        );
    }
}
