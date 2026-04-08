//! Integration tests for token estimation accuracy.
//!
//! Validates that heuristic token counting produces estimates within
//! acceptable error bounds for different content types.

use pipit_provider::types::{ContentBlock, Message, MessageMetadata, Role, TokenCount};

fn make_text_message(role: Role, text: &str) -> Message {
    Message {
        role,
        content: vec![ContentBlock::Text(text.to_string())],
        metadata: MessageMetadata::default(),
    }
}

/// English prose: expected ratio ~3.5-4.0 chars/token.
/// 1000 chars ≈ 250-285 tokens.
#[test]
fn english_prose_estimation() {
    let prose = "The quick brown fox jumps over the lazy dog. ".repeat(22); // ~990 chars
    let msg = make_text_message(Role::User, &prose);
    let estimated = msg.estimated_tokens();
    // 990 chars / 4.0 ≈ 248, / 3.5 ≈ 283
    assert!(
        estimated >= 200 && estimated <= 350,
        "English prose estimate {} out of expected range [200, 350] for {} chars",
        estimated,
        prose.len()
    );
}

/// Code with high punctuation density: expected ratio ~3.0 chars/token.
/// 1000 chars of JSON ≈ 333 tokens.
#[test]
fn code_estimation() {
    let code = r#"{"name":"value","array":[1,2,3],"nested":{"key":"val"}}"#.repeat(18); // ~972 chars
    let msg = make_text_message(Role::User, &code);
    let estimated = msg.estimated_tokens();
    // 972 chars / 3.0 ≈ 324
    assert!(
        estimated >= 250 && estimated <= 450,
        "Code estimate {} out of expected range [250, 450] for {} chars",
        estimated,
        code.len()
    );
}

/// Empty content should produce 0 tokens.
#[test]
fn empty_content_estimation() {
    let msg = make_text_message(Role::User, "");
    assert_eq!(msg.estimated_tokens(), 0);
}

/// Tool call arguments are counted.
#[test]
fn tool_call_estimation() {
    let msg = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolCall {
            call_id: "call_1".to_string(),
            name: "read_file".to_string(),
            args: serde_json::json!({"path": "/some/file.rs", "start_line": 1, "end_line": 100}),
        }],
        metadata: MessageMetadata::default(),
    };
    let estimated = msg.estimated_tokens();
    assert!(
        estimated > 5 && estimated < 100,
        "Tool call estimate {} should be in [5, 100]",
        estimated
    );
}

/// Tool results are counted.
#[test]
fn tool_result_estimation() {
    let big_result = "fn main() {\n    println!(\"hello\");\n}\n".repeat(50);
    let msg = Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            call_id: "call_1".to_string(),
            content: big_result.clone(),
            is_error: false,
        }],
        metadata: MessageMetadata::default(),
    };
    let estimated = msg.estimated_tokens();
    assert!(
        estimated > 100,
        "Large tool result ({} chars) should estimate > 100 tokens, got {}",
        big_result.len(),
        estimated
    );
}

/// Multi-message conversations accumulate correctly.
#[test]
fn multi_message_estimation() {
    let msgs = vec![
        make_text_message(Role::User, "Hello, please help me with this task."),
        make_text_message(
            Role::Assistant,
            "Sure, I can help. Let me analyze the code first.",
        ),
        make_text_message(Role::User, "Great, here is the code: fn main() {}"),
    ];
    let total: u64 = msgs.iter().map(|m| m.estimated_tokens()).sum();
    assert!(
        total > 20 && total < 100,
        "Multi-message total {} should be in [20, 100]",
        total
    );
}
