//! The provider-neutral conversation model.
//!
//! OpenAI, DeepSeek and Anthropic all do tool-calling, but their wire formats disagree on
//! nearly every detail (a system prompt is a message for OpenAI and a top-level field for
//! Anthropic; tool calls are `tool_calls` with stringified arguments for OpenAI and
//! `tool_use` content blocks for Anthropic). The rest of the app speaks only these neutral
//! types; each adapter in `provider.rs` translates them to and from its own JSON.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    /// A turn carrying the results of tool calls back to the model.
    Tool,
}

/// The model asking to run one tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// Provider-assigned id, echoed back with the result so it can be paired.
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// What a tool produced, on its way back to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub id: String,
    pub name: String,
    pub content: String,
}

/// One turn of the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMsg {
    pub role: Role,
    pub text: String,
    /// Present on an assistant turn that asked for tools.
    pub tool_calls: Vec<ToolCall>,
    /// Present on a `Tool` turn feeding results back.
    pub tool_results: Vec<ToolResult>,
}

impl ChatMsg {
    pub fn system(text: impl Into<String>) -> Self {
        Self::plain(Role::System, text)
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::plain(Role::User, text)
    }

    fn plain(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    pub fn assistant(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
            tool_calls,
            tool_results: Vec::new(),
        }
    }

    pub fn tool_results(results: Vec<ToolResult>) -> Self {
        Self {
            role: Role::Tool,
            text: String::new(),
            tool_calls: Vec::new(),
            tool_results: results,
        }
    }
}

/// What a completion returns: the assistant's text, and any tools it wants run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assistant {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
}

impl Assistant {
    pub fn wants_tools(&self) -> bool {
        !self.tool_calls.is_empty()
    }
}

/// A callable the model is told about: name, human description, and a JSON-schema object
/// describing its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}
