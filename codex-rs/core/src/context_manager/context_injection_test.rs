//! Test for automatic codebase context injection

use super::CodebaseContext;
use super::CodebaseSearchProvider;
use super::ContextManager;
use codex_codebase_context::ContextSearchMetadata;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Mock provider for testing
#[derive(Debug)]
struct MockCodebaseProvider {
    should_return_context: bool,
}

impl CodebaseSearchProvider for MockCodebaseProvider {
    fn provide_context<'a>(
        &'a mut self,
        query: &'a str,
        _token_budget: usize,
        _metadata: Option<&'a ContextSearchMetadata>,
    ) -> BoxFuture<'a, anyhow::Result<Option<CodebaseContext>>> {
        Box::pin(async move {
            if self.should_return_context && query.contains("show me") {
                Ok(Some(CodebaseContext {
                    formatted_context: "Mock context from codebase".to_string(),
                    chunks_count: 5,
                    tokens_used: 150,
                }))
            } else {
                Ok(None)
            }
        })
    }
}

#[tokio::test]
async fn test_context_injection_with_trigger() {
    // Create provider that returns context
    let provider = MockCodebaseProvider {
        should_return_context: true,
    };
    let boxed: Box<dyn CodebaseSearchProvider> = Box::new(provider);
    let provider_arc = Arc::new(Mutex::new(boxed));

    // Create ContextManager with provider
    let mut manager = ContextManager::new_with_config(
        Some(provider_arc),
        crate::config::types::CodebaseSearchConfig {
            enabled: true,
            ..Default::default()
        },
    );

    // Create user message with trigger keyword
    let user_message = ResponseItem::Message {
        id: Some("test".to_string()),
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "show me error handling".to_string(),
        }],
    };

    // Record with context injection
    manager
        .record_items_with_context([&user_message], false, None)
        .await
        .unwrap();

    // Check that TWO items were recorded: context entry + user message
    let history = manager.get_history();
    assert_eq!(
        history.len(),
        2,
        "Should have 2 items: injected context + user message"
    );

    // First item should be context entry with codebase metadata
    if let ResponseItem::Context {
        formatted_context,
        chunks_count,
        tokens_used,
    } = &history[0]
    {
        assert!(
            formatted_context.contains("Relevant Codebase Context"),
            "Context body should include section header"
        );
        assert!(
            formatted_context.contains("Mock context from codebase"),
            "Context body should contain actual context"
        );
        assert_eq!(*chunks_count, 5);
        assert_eq!(*tokens_used, 150);
    } else {
        panic!("First item should be Context");
    }

    // Second item should be original user message
    if let ResponseItem::Message { role, .. } = &history[1] {
        assert_eq!(role, "user", "Second message should be user role");
    } else {
        panic!("Second item should be Message");
    }
}

#[tokio::test]
async fn test_no_injection_without_trigger() {
    // Create provider
    let provider = MockCodebaseProvider {
        should_return_context: true,
    };
    let boxed: Box<dyn CodebaseSearchProvider> = Box::new(provider);
    let provider_arc = Arc::new(Mutex::new(boxed));

    // Create ContextManager
    let mut manager = ContextManager::new_with_config(
        Some(provider_arc),
        crate::config::types::CodebaseSearchConfig {
            enabled: true,
            ..Default::default()
        },
    );

    // Create user message WITHOUT trigger keyword
    let user_message = ResponseItem::Message {
        id: Some("test".to_string()),
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "thank you".to_string(), // No trigger
        }],
    };

    // Record
    manager
        .record_items_with_context([&user_message], false, None)
        .await
        .unwrap();

    // Should only have user message (no context injection)
    let history = manager.get_history();
    assert_eq!(history.len(), 1, "Should only have user message");

    if let ResponseItem::Message { role, .. } = &history[0] {
        assert_eq!(role, "user");
    }
}

#[tokio::test]
async fn test_disabled_config_no_injection() {
    // Create provider
    let provider = MockCodebaseProvider {
        should_return_context: true,
    };
    let boxed: Box<dyn CodebaseSearchProvider> = Box::new(provider);
    let provider_arc = Arc::new(Mutex::new(boxed));

    // Create ContextManager with DISABLED config
    let mut manager = ContextManager::new_with_config(
        Some(provider_arc),
        crate::config::types::CodebaseSearchConfig {
            enabled: false, // DISABLED
            ..Default::default()
        },
    );

    // Create user message with trigger
    let user_message = ResponseItem::Message {
        id: Some("test".to_string()),
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "show me error handling".to_string(),
        }],
    };

    // Record
    manager
        .record_items_with_context([&user_message], false, None)
        .await
        .unwrap();

    // Should NOT inject context (config disabled)
    let history = manager.get_history();
    assert_eq!(
        history.len(),
        1,
        "Should only have user message (config disabled)"
    );
}
