use codex_core::features::Feature;
use codex_core::protocol::AgentStatus;
use codex_core::protocol::CollabAgentInteractionBeginEvent;
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
async fn collab_routing_agent_role_all() {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collab_message_schema_mentions_and_priority() {
    let server = start_mock_server().await;
    let _responses = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_response_created("resp-priority"),
            ev_assistant_message("msg-priority", "ack"),
            ev_completed("resp-priority"),
        ])],
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

    let message = "@scout [priority:urgent] validate slice S4".to_string();
    let submission_id = test
        .codex
        .submit(Op::CollabSendInput {
            receiver_thread_ids: vec![receiver_thread_id],
            items: vec![UserInput::Text {
                text: message.clone(),
                text_elements: Vec::new(),
            }],
        })
        .await
        .expect("submit collab op");

    let begin = wait_for_event_match(test.codex.as_ref(), |event| match event {
        EventMsg::CollabAgentInteractionBegin(CollabAgentInteractionBeginEvent {
            call_id,
            receiver_thread_id: event_receiver_thread_id,
            prompt,
            message,
            ..
        }) => Some((
            call_id.clone(),
            *event_receiver_thread_id,
            prompt.clone(),
            message.clone(),
        )),
        _ => None,
    })
    .await;
    assert_eq!(begin.0, submission_id);
    assert_eq!(begin.1, receiver_thread_id);
    assert_eq!(begin.2, message);
    assert_eq!(begin.3.author, "user".to_string());
    assert_eq!(begin.3.role, "user".to_string());
    assert_eq!(begin.3.status, "running".to_string());
    assert_eq!(begin.3.priority, "urgent".to_string());
    assert_eq!(begin.3.intent, message);
    assert_eq!(begin.3.mentions, vec!["@scout".to_string()]);

    let end = wait_for_event_match(test.codex.as_ref(), |event| match event {
        EventMsg::CollabWaitingEnd(CollabWaitingEndEvent {
            statuses, call_id, ..
        }) => Some((call_id.clone(), statuses.clone())),
        _ => None,
    })
    .await;

    assert_eq!(end.0, submission_id);
    assert_eq!(
        end.1.get(&receiver_thread_id),
        Some(&AgentStatus::Completed(Some("ack".to_string())))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collab_message_priority_becomes_urgent_for_split_structured_intervention() {
    let server = start_mock_server().await;
    let _responses = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_response_created("resp-urgent"),
            ev_assistant_message("msg-urgent", "ack"),
            ev_completed("resp-urgent"),
        ])],
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

    let submission_id = test
        .codex
        .submit(Op::CollabSendInput {
            receiver_thread_ids: vec![receiver_thread_id],
            items: vec![
                UserInput::Mention {
                    name: "scout".to_string(),
                    path: "app://scout".to_string(),
                },
                UserInput::Text {
                    text: "please stop and regroup".to_string(),
                    text_elements: Vec::new(),
                },
            ],
        })
        .await
        .expect("submit collab op");

    let begin = wait_for_event_match(test.codex.as_ref(), |event| match event {
        EventMsg::CollabAgentInteractionBegin(CollabAgentInteractionBeginEvent {
            call_id,
            receiver_thread_id: event_receiver_thread_id,
            message,
            ..
        }) => Some((call_id.clone(), *event_receiver_thread_id, message.clone())),
        _ => None,
    })
    .await;

    assert_eq!(begin.0, submission_id);
    assert_eq!(begin.1, receiver_thread_id);
    assert_eq!(begin.2.priority, "urgent".to_string());

    let end = wait_for_event_match(test.codex.as_ref(), |event| match event {
        EventMsg::CollabWaitingEnd(CollabWaitingEndEvent {
            statuses, call_id, ..
        }) => Some((call_id.clone(), statuses.clone())),
        _ => None,
    })
    .await;

    assert_eq!(end.0, submission_id);
    assert_eq!(
        end.1.get(&receiver_thread_id),
        Some(&AgentStatus::Completed(Some("ack".to_string())))
    );
}
