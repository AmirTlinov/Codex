use codex_core::features::Feature;
use codex_core::protocol::AgentStatus;
use codex_core::protocol::CollabWaitingBeginEvent;
use codex_core::protocol::CollabWaitingEndEvent;
use codex_core::protocol::EventMsg;
use codex_core::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collab_send_input_emits_wait_results_as_events() {
    let server = start_mock_server().await;
    let _responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp1"),
                ev_assistant_message("msg-1", "agent reply"),
                ev_completed("resp1"),
            ]),
            sse(vec![
                ev_response_created("resp2"),
                ev_assistant_message("msg-2", "agent reply"),
                ev_completed("resp2"),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Collab);
    });
    let test = builder.build(&server).await.expect("build test codex");

    let receiver = test
        .thread_manager
        .start_thread(test.config.clone())
        .await
        .expect("start receiver thread");
    let receiver_thread_id = receiver.thread_id;

    let receiver2 = test
        .thread_manager
        .start_thread(test.config.clone())
        .await
        .expect("start receiver thread");
    let receiver2_thread_id = receiver2.thread_id;

    let submission_id = test
        .codex
        .submit(Op::CollabSendInput {
            receiver_thread_ids: vec![receiver_thread_id, receiver2_thread_id],
            items: vec![UserInput::Text {
                text: "@agent hello".to_string(),
                text_elements: Vec::new(),
            }],
        })
        .await
        .expect("submit collab op");

    let begin = wait_for_event_match(test.codex.as_ref(), |event| match event {
        EventMsg::CollabWaitingBegin(CollabWaitingBeginEvent { call_id, .. }) => {
            Some(call_id.clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(begin, submission_id);

    let end = wait_for_event_match(test.codex.as_ref(), |event| match event {
        EventMsg::CollabWaitingEnd(CollabWaitingEndEvent {
            statuses, call_id, ..
        }) => Some((call_id.clone(), statuses.clone())),
        _ => None,
    })
    .await;

    assert_eq!(end.0, submission_id);
    assert_eq!(end.1.len(), 2);
    assert_eq!(
        end.1.get(&receiver_thread_id),
        Some(&AgentStatus::Completed(Some("agent reply".to_string())))
    );
    assert_eq!(
        end.1.get(&receiver2_thread_id),
        Some(&AgentStatus::Completed(Some("agent reply".to_string())))
    );
}
