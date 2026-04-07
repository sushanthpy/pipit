//! Agent Interaction Tools — ask_user (structured human-in-the-loop).
//!
//! Typed, option-based user interaction with Select/MultiSelect/FreeText/Confirm.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::typed_tool::*;
use crate::{ToolContext, ToolError};

/// Question format for ask_user.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// Single selection from options.
    Select,
    /// Multiple selection from options.
    MultiSelect,
    /// Free-text response.
    FreeText,
    /// Yes/no confirmation.
    Confirm,
}

/// Input for the ask_user tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskUserInput {
    /// The question to ask the user.
    pub question: String,
    /// Optional list of choices.
    #[serde(default)]
    pub options: Vec<String>,
    /// Kind of response expected.
    #[serde(default = "default_question_kind")]
    pub kind: QuestionKind,
}

fn default_question_kind() -> QuestionKind {
    QuestionKind::FreeText
}

/// Ask the user a structured question. Blocks the turn.
pub struct AskUserTool;

#[async_trait]
impl TypedTool for AskUserTool {
    type Input = AskUserInput;
    const NAME: &'static str = "ask_user";
    const CAPABILITIES: CapabilitySet = CapabilitySet(CapabilitySet::USER_INTERACTION.0);
    const PURITY: Purity = Purity::Pure;

    fn describe() -> ToolCard {
        ToolCard {
            name: "ask_user".into(),
            summary: "Ask the user a question and wait for their response".into(),
            when_to_use: "When you need clarification, confirmation, or a decision from the user before proceeding. Use sparingly — only for genuinely ambiguous situations.".into(),
            examples: vec![
                ToolExample {
                    description: "Ask for confirmation".into(),
                    input: serde_json::json!({
                        "question": "Should I delete the unused functions?",
                        "kind": "confirm"
                    }),
                },
                ToolExample {
                    description: "Ask to choose from options".into(),
                    input: serde_json::json!({
                        "question": "Which database adapter?",
                        "options": ["PostgreSQL", "SQLite", "MySQL"],
                        "kind": "select"
                    }),
                },
            ],
            tags: vec!["user".into(), "interaction".into(), "question".into(), "confirmation".into()],
            purity: Purity::Pure,
            capabilities: CapabilitySet::USER_INTERACTION.0,
        }
    }

    async fn execute(
        &self,
        input: AskUserInput,
        _ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError> {
        // Format the question for display.
        // The actual user interaction is handled by the agent loop's
        // ApprovalHandler — this tool surfaces the question through the
        // standard tool result, and the TUI/CLI presents it.
        let mut formatted = format!("**Question for user:** {}\n", input.question);

        if !input.options.is_empty() {
            formatted.push_str("\nOptions:\n");
            for (i, opt) in input.options.iter().enumerate() {
                formatted.push_str(&format!("  {}. {}\n", i + 1, opt));
            }
        }

        match input.kind {
            QuestionKind::Confirm => {
                formatted.push_str("\n(Waiting for yes/no confirmation)");
            }
            QuestionKind::Select => {
                formatted.push_str("\n(Waiting for selection)");
            }
            QuestionKind::MultiSelect => {
                formatted.push_str("\n(Waiting for one or more selections)");
            }
            QuestionKind::FreeText => {
                formatted.push_str("\n(Waiting for free-text response)");
            }
        }

        // The result surfaces through the event stream. The agent loop
        // will pause for user input when it sees this tool's output.
        Ok(TypedToolResult::text(formatted))
    }
}
