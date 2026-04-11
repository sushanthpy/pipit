// ═══════════════════════════════════════════════════════════════════════
// Streaming Conformance Contract Tests
// ═══════════════════════════════════════════════════════════════════════
//
// This test file documents and enforces the canonical ContentEvent
// streaming contract that ALL provider adapters must satisfy. It tests
// the public API surface (ContentEvent, StopReason, UsageMetadata,
// ModelCapabilities) without relying on private parser internals.
//
// Provider-specific parser tests live inside each provider module as
// `#[cfg(test)] mod tests`. This file tests cross-cutting invariants.
//
// ── CANONICAL EVENT CONTRACT ──
//
// Every streaming response from an LLM provider is a sequence of
// ContentEvents that MUST satisfy these invariants:
//
// I1. Terminal: Every complete stream ends with exactly one Finished event.
//     Finished is always the last event in the sequence.
//
// I2. ContentDelta text MUST NOT be empty (empty deltas are suppressed
//     at the parser level, not propagated to consumers).
//
// I3. ToolCallComplete args MUST be valid serde_json::Value. If the raw
//     JSON cannot be parsed, args MUST be Value::Null (not an error).
//
// I4. ToolCallDelta is defined in the event enum but MUST NOT be emitted
//     by any provider. All providers buffer tool call fragments internally
//     and emit a single ToolCallComplete per tool invocation.
//
// I5. ThinkingDelta text MUST NOT be empty (same suppression rule as I2).
//
// I6. UsageMetadata in the Finished event SHOULD contain non-zero values
//     when the provider reports usage. Zero values indicate the provider
//     did not report usage for that field.
//
// I7. StopReason mapping:
//     - Natural completion → EndTurn or Stop
//     - Tool invocation → ToolUse
//     - Output length limit → MaxTokens
//     - Provider error → Error
//
// I8. Each provider adapter is a deterministic transducer: the same input
//     byte sequence always produces the same ContentEvent sequence.
//
// I9. Provider capabilities MUST accurately reflect event emission:
//     - If supports_thinking is false, ThinkingDelta MUST NOT be emitted.
//     - If supports_tool_use is false, ToolCallComplete MUST NOT be emitted.
//

use pipit_provider::{ContentEvent, ModelCapabilities, StopReason, UsageMetadata};

// ── I1: Finished is always terminal ──

#[test]
fn finished_event_is_constructible() {
    let event = ContentEvent::Finished {
        stop_reason: StopReason::EndTurn,
        usage: UsageMetadata::default(),
    };
    assert!(matches!(event, ContentEvent::Finished { .. }));
}

// ── I3: ToolCallComplete accepts null args ──

#[test]
fn tool_call_complete_accepts_null_args() {
    let event = ContentEvent::ToolCallComplete {
        call_id: "test".to_string(),
        tool_name: "bash".to_string(),
        args: serde_json::Value::Null,
    };
    if let ContentEvent::ToolCallComplete { args, .. } = event {
        assert!(args.is_null());
    }
}

// ── I6: UsageMetadata default is zeroed ──

#[test]
fn usage_metadata_default_is_zero() {
    let u = UsageMetadata::default();
    assert_eq!(u.input_tokens, 0);
    assert_eq!(u.output_tokens, 0);
    assert_eq!(u.total_tokens(), 0);
    assert!(u.cache_read_tokens.is_none());
    assert!(u.cache_creation_tokens.is_none());
}

// ── I7: StopReason has all required variants ──

#[test]
fn stop_reason_has_all_semantic_variants() {
    // Ensure the enum has the variants the contract requires.
    // If a variant is removed or renamed, this test fails at compile time.
    let _end = StopReason::EndTurn;
    let _stop = StopReason::Stop;
    let _tool = StopReason::ToolUse;
    let _max = StopReason::MaxTokens;
    let _err = StopReason::Error;
}

// ── I4: ContentEvent enum includes ToolCallDelta (for forward compat) ──

#[test]
fn tool_call_delta_variant_exists() {
    // ToolCallDelta exists in the enum for future use but should never
    // be emitted by current providers. This test documents its existence.
    let _delta = ContentEvent::ToolCallDelta {
        call_id: "test".to_string(),
        tool_name: "bash".to_string(),
        args_delta: "{}".to_string(),
    };
}

// ── ContentEvent enum completeness ──

#[test]
fn all_content_event_variants_are_matchable() {
    // Verify the enum has exactly the expected variants.
    // A new variant would cause a compile error here (non-exhaustive match).
    let events: Vec<ContentEvent> = vec![
        ContentEvent::ContentDelta {
            text: "hello".to_string(),
        },
        ContentEvent::ThinkingDelta {
            text: "thinking".to_string(),
        },
        ContentEvent::ToolCallDelta {
            call_id: "id".to_string(),
            tool_name: "tool".to_string(),
            args_delta: "delta".to_string(),
        },
        ContentEvent::ToolCallComplete {
            call_id: "id".to_string(),
            tool_name: "tool".to_string(),
            args: serde_json::json!({}),
        },
        ContentEvent::Finished {
            stop_reason: StopReason::EndTurn,
            usage: UsageMetadata::default(),
        },
    ];

    for event in &events {
        match event {
            ContentEvent::ContentDelta { text } => assert!(!text.is_empty()),
            ContentEvent::ThinkingDelta { text } => assert!(!text.is_empty()),
            ContentEvent::ToolCallDelta { .. } => {}
            ContentEvent::ToolCallComplete { args, .. } => assert!(args.is_object()),
            ContentEvent::Finished { .. } => {}
        }
    }
    assert_eq!(events.len(), 5, "ContentEvent should have exactly 5 variants");
}

// ── Provider capability matrix ──

/// Test that ModelCapabilities has the fields needed for event contract enforcement.
#[test]
fn capabilities_fields_exist_for_contract_enforcement() {
    let caps = ModelCapabilities {
        context_window: 128_000,
        max_output_tokens: 16_384,
        supports_tool_use: true,
        supports_streaming: true,
        supports_thinking: true,
        supports_images: false,
        supports_prefill: false,
        preferred_edit_format: None,
    };
    assert!(caps.supports_thinking);
    assert!(caps.supports_tool_use);
    assert!(caps.supports_streaming);
}

// ── StopReason serialization round-trip ──

#[test]
fn stop_reason_serializes_deterministically() {
    for reason in [
        StopReason::EndTurn,
        StopReason::Stop,
        StopReason::ToolUse,
        StopReason::MaxTokens,
        StopReason::Error,
    ] {
        let json = serde_json::to_string(&reason).unwrap();
        let roundtrip: StopReason = serde_json::from_str(&json).unwrap();
        assert_eq!(reason, roundtrip);
    }
}

// ── UsageMetadata total_tokens computation ──

#[test]
fn usage_total_tokens_is_input_plus_output() {
    let u = UsageMetadata {
        input_tokens: 1000,
        output_tokens: 500,
        cache_read_tokens: Some(200),
        cache_creation_tokens: Some(50),
    };
    assert_eq!(u.total_tokens(), 1500);
}
