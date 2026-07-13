//! The AI troubleshooting assistant: a chat panel that can inspect the project and drive
//! log-scouter's own operations (filters, searches, sources) on the user's behalf.
//!
//! The network call is the only thing that leaves the main thread. A worker thread owns a
//! tokio runtime and talks to the provider; the main thread executes any tool calls the
//! model asks for against `AppState`, feeds the results back, and loops until the model
//! stops calling tools. Keeping all state mutation on the main thread means tool execution
//! reuses the existing mutators, so the panels refresh for free.

pub mod config;
pub mod message;
pub mod provider;
pub mod tools;
pub mod worker;

pub use config::{AiConfig, Provider};
pub use message::{Assistant, ChatMsg, Role, ToolCall, ToolResult, ToolSpec};
pub use worker::{AgentEvent, AgentRequest, AiWorker};
