//! Talking to the three providers.
//!
//! DeepSeek and OpenAI share the `/chat/completions` wire format, so one adapter serves
//! both (they differ only in base URL and model). Anthropic uses the Messages API. The
//! request-body builders and the response parsers are pure functions so they can be
//! round-tripped against captured JSON without a network; `complete` is the only part that
//! touches reqwest.
//!
//! Rust has no official Anthropic SDK, so this is the raw HTTP shape from the Messages API:
//! `x-api-key` + `anthropic-version` headers, a top-level `system` string, `tools` as
//! `{name, description, input_schema}`, and `tool_use` / `tool_result` content blocks.

use crate::ai::config::AiConfig;
use crate::ai::message::{Assistant, ChatMsg, Role, ToolCall, ToolSpec};
use serde_json::{json, Value};
use std::time::Duration;

/// Anthropic pins the wire format to a dated version.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Output cap. Generous enough for a chatty tool-calling turn without risking a timeout.
const MAX_TOKENS: u32 = 4096;

/// Run one completion: send the conversation, get back the assistant's text and any tool
/// calls. Blocking is fine -- the worker thread owns the runtime and calls this via
/// `block_on`.
pub async fn complete(
    client: &reqwest::Client,
    config: &AiConfig,
    key: &str,
    conversation: &[ChatMsg],
    tools: &[ToolSpec],
) -> Result<Assistant, String> {
    let model = config.model();
    let base = config.base_url();
    if config.provider.is_anthropic() {
        let body = anthropic_body(&model, conversation, tools);
        let value = post(
            client,
            &format!("{base}/messages"),
            body,
            &[("x-api-key", key), ("anthropic-version", ANTHROPIC_VERSION)],
        )
        .await?;
        parse_anthropic(&value)
    } else {
        let body = openai_body(&model, conversation, tools);
        let value = post(
            client,
            &format!("{base}/chat/completions"),
            body,
            &[("authorization", &format!("Bearer {key}"))],
        )
        .await?;
        parse_openai(&value)
    }
}

async fn post(
    client: &reqwest::Client,
    url: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> Result<Value, String> {
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .timeout(Duration::from_secs(120))
        .json(&body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }

    let response = request.send().await.map_err(|error| error.to_string())?;
    let status = response.status();
    let text = response.text().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(provider_error(status.as_u16(), &text));
    }
    serde_json::from_str(&text).map_err(|error| format!("bad response JSON: {error}"))
}

/// Pull a human-readable message out of an error body, falling back to the raw text.
fn provider_error(status: u16, body: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(200).collect());
    format!("provider error {status}: {message}")
}

// ---- OpenAI / DeepSeek wire format --------------------------------------------------

pub fn openai_body(model: &str, conversation: &[ChatMsg], tools: &[ToolSpec]) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for msg in conversation {
        match msg.role {
            Role::System => messages.push(json!({"role": "system", "content": msg.text})),
            Role::User => messages.push(json!({"role": "user", "content": msg.text})),
            Role::Assistant => {
                let mut entry = json!({ "role": "assistant" });
                entry["content"] = if msg.text.is_empty() {
                    Value::Null
                } else {
                    Value::String(msg.text.clone())
                };
                if !msg.tool_calls.is_empty() {
                    entry["tool_calls"] = msg
                        .tool_calls
                        .iter()
                        .map(|call| {
                            json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    // OpenAI wants the arguments as a JSON *string*.
                                    "arguments": call.arguments.to_string(),
                                },
                            })
                        })
                        .collect();
                }
                messages.push(entry);
            }
            // Each result is its own `tool` message, paired by id.
            Role::Tool => {
                for result in &msg.tool_results {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": result.id,
                        "content": result.content,
                    }));
                }
            }
        }
    }

    let mut body = json!({ "model": model, "messages": messages });
    if !tools.is_empty() {
        body["tools"] = tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    },
                })
            })
            .collect();
        body["tool_choice"] = json!("auto");
    }
    body
}

pub fn parse_openai(value: &Value) -> Result<Assistant, String> {
    let message = value
        .pointer("/choices/0/message")
        .ok_or("no choices in response")?;
    let text = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let mut tool_calls = Vec::new();
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let function = call.get("function");
            let name = function
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // `arguments` is a JSON string; an empty one means no arguments.
            let raw = function
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let arguments = if raw.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
            };
            tool_calls.push(ToolCall {
                id,
                name,
                arguments,
            });
        }
    }
    Ok(Assistant { text, tool_calls })
}

// ---- Anthropic Messages API ---------------------------------------------------------

pub fn anthropic_body(model: &str, conversation: &[ChatMsg], tools: &[ToolSpec]) -> Value {
    // Anthropic carries the system prompt in a top-level field, not as a message.
    let system = conversation
        .iter()
        .filter(|msg| msg.role == Role::System)
        .map(|msg| msg.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut messages: Vec<Value> = Vec::new();
    for msg in conversation {
        match msg.role {
            Role::System => {}
            Role::User => messages.push(json!({"role": "user", "content": msg.text})),
            Role::Assistant => {
                let mut blocks: Vec<Value> = Vec::new();
                if !msg.text.is_empty() {
                    blocks.push(json!({"type": "text", "text": msg.text}));
                }
                for call in &msg.tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments,
                    }));
                }
                messages.push(json!({"role": "assistant", "content": blocks}));
            }
            // Tool results ride back inside a user turn as `tool_result` blocks.
            Role::Tool => {
                let blocks: Vec<Value> = msg
                    .tool_results
                    .iter()
                    .map(|result| {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": result.id,
                            "content": result.content,
                        })
                    })
                    .collect();
                messages.push(json!({"role": "user", "content": blocks}));
            }
        }
    }

    let mut body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "messages": messages,
    });
    if !system.is_empty() {
        body["system"] = Value::String(system);
    }
    if !tools.is_empty() {
        body["tools"] = tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.parameters,
                })
            })
            .collect();
    }
    body
}

pub fn parse_anthropic(value: &Value) -> Result<Assistant, String> {
    let blocks = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or("no content in response")?;

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(chunk) = block.get("text").and_then(Value::as_str) {
                    text.push_str(chunk);
                }
            }
            Some("tool_use") => {
                tool_calls.push(ToolCall {
                    id: block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    arguments: block.get("input").cloned().unwrap_or_else(|| json!({})),
                });
            }
            _ => {}
        }
    }
    Ok(Assistant { text, tool_calls })
}
