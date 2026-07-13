use log_scouter::ai::config::{AiConfig, Provider};
use log_scouter::ai::message::{ChatMsg, ToolCall, ToolResult, ToolSpec};
use log_scouter::ai::provider;
use serde_json::json;

fn tool() -> ToolSpec {
    ToolSpec {
        name: "count_matches".into(),
        description: "count".into(),
        parameters: json!({"type": "object", "properties": {"query": {"type": "string"}}}),
    }
}

/// A short conversation: system, user, an assistant tool call, and the tool result.
fn conversation() -> Vec<ChatMsg> {
    vec![
        ChatMsg::system("you are helpful"),
        ChatMsg::user("how many errors?"),
        ChatMsg::assistant(
            "let me check",
            vec![ToolCall {
                id: "call_1".into(),
                name: "count_matches".into(),
                arguments: json!({"query": "error"}),
            }],
        ),
        ChatMsg::tool_results(vec![ToolResult {
            id: "call_1".into(),
            name: "count_matches".into(),
            content: "12 of 40 match".into(),
        }]),
    ]
}

// ---- OpenAI / DeepSeek wire format --------------------------------------------------

#[test]
fn openai_body_maps_every_turn_and_tools() {
    let body = provider::openai_body("gpt-4o", &conversation(), &[tool()]);
    assert_eq!(body["model"], "gpt-4o");

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");

    // The assistant turn keeps its text and carries the call with stringified arguments.
    let assistant = &messages[2];
    assert_eq!(assistant["role"], "assistant");
    let call = &assistant["tool_calls"][0];
    assert_eq!(call["id"], "call_1");
    assert_eq!(call["function"]["name"], "count_matches");
    assert_eq!(call["function"]["arguments"], "{\"query\":\"error\"}");

    // The result is its own `tool` message, paired by id.
    let result = &messages[3];
    assert_eq!(result["role"], "tool");
    assert_eq!(result["tool_call_id"], "call_1");
    assert_eq!(result["content"], "12 of 40 match");

    // Tools are function specs, with auto tool_choice.
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "count_matches");
    assert_eq!(body["tool_choice"], "auto");
}

#[test]
fn parse_openai_reads_text_and_tool_calls() {
    let value = json!({
        "choices": [{"message": {
            "content": "Let me check.",
            "tool_calls": [{
                "id": "call_9",
                "type": "function",
                "function": {"name": "count_matches", "arguments": "{\"query\":\"timeout\"}"},
            }],
        }}]
    });
    let assistant = provider::parse_openai(&value).unwrap();
    assert_eq!(assistant.text, "Let me check.");
    assert_eq!(assistant.tool_calls.len(), 1);
    assert_eq!(assistant.tool_calls[0].id, "call_9");
    assert_eq!(assistant.tool_calls[0].name, "count_matches");
    assert_eq!(assistant.tool_calls[0].arguments["query"], "timeout");

    // A plain reply with no tools.
    let plain = json!({"choices": [{"message": {"content": "All clear."}}]});
    let assistant = provider::parse_openai(&plain).unwrap();
    assert_eq!(assistant.text, "All clear.");
    assert!(!assistant.wants_tools());
}

// ---- Anthropic Messages API ---------------------------------------------------------

#[test]
fn anthropic_body_lifts_system_and_uses_content_blocks() {
    let body = provider::anthropic_body("claude-opus-4-8", &conversation(), &[tool()]);
    assert_eq!(body["model"], "claude-opus-4-8");
    assert!(body["max_tokens"].as_u64().unwrap() > 0);
    // The system prompt is a top-level field, not a message.
    assert_eq!(body["system"], "you are helpful");

    let messages = body["messages"].as_array().unwrap();
    // System is lifted out, so the first message is the user turn.
    assert_eq!(messages[0]["role"], "user");

    // The assistant turn is text + tool_use content blocks.
    let assistant = &messages[1];
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["content"][0]["type"], "text");
    let use_block = &assistant["content"][1];
    assert_eq!(use_block["type"], "tool_use");
    assert_eq!(use_block["id"], "call_1");
    assert_eq!(use_block["input"]["query"], "error");

    // The result rides back inside a user turn as a tool_result block.
    let result = &messages[2];
    assert_eq!(result["role"], "user");
    assert_eq!(result["content"][0]["type"], "tool_result");
    assert_eq!(result["content"][0]["tool_use_id"], "call_1");

    // Tools use `input_schema`, not `parameters`.
    assert_eq!(body["tools"][0]["name"], "count_matches");
    assert!(body["tools"][0]["input_schema"].is_object());
}

#[test]
fn parse_anthropic_gathers_text_and_tool_use_blocks() {
    let value = json!({
        "content": [
            {"type": "text", "text": "Checking."},
            {"type": "tool_use", "id": "tu_1", "name": "add_filter",
             "input": {"field": "level", "op": "equals", "value": "Trace", "action": "exclude"}},
        ],
        "stop_reason": "tool_use",
    });
    let assistant = provider::parse_anthropic(&value).unwrap();
    assert_eq!(assistant.text, "Checking.");
    assert_eq!(assistant.tool_calls.len(), 1);
    assert_eq!(assistant.tool_calls[0].name, "add_filter");
    assert_eq!(assistant.tool_calls[0].arguments["value"], "Trace");
}

// ---- config -------------------------------------------------------------------------

#[test]
fn provider_labels_env_vars_and_wire_family() {
    assert_eq!(Provider::from_label("anthropic"), Some(Provider::Anthropic));
    assert_eq!(Provider::from_label("DeepSeek"), Some(Provider::Deepseek));
    assert_eq!(Provider::from_label("bogus"), None);

    assert_eq!(Provider::OpenAi.key_var(), "OPENAI_API_KEY");
    assert_eq!(Provider::Anthropic.key_var(), "ANTHROPIC_API_KEY");
    // DeepSeek shares the OpenAI-compatible wire format; only Anthropic differs.
    assert!(Provider::Anthropic.is_anthropic());
    assert!(!Provider::Deepseek.is_anthropic());
}

#[test]
fn config_model_defaults_and_key_from_env() {
    let mut config = AiConfig {
        provider: Provider::Anthropic,
        model: String::new(),
        api_key: String::new(),
    };
    // Empty model falls back to the provider default.
    assert_eq!(config.model(), "claude-opus-4-8");
    config.model = "claude-haiku-4-5".into();
    assert_eq!(config.model(), "claude-haiku-4-5");

    // The key is read from the environment; a blank one is treated as unset.
    std::env::set_var("ANTHROPIC_API_KEY", "  ");
    assert_eq!(config.api_key(), None);
    std::env::set_var("ANTHROPIC_API_KEY", "sk-test");
    assert_eq!(config.api_key().as_deref(), Some("sk-test"));
    std::env::remove_var("ANTHROPIC_API_KEY");

    // With no env var, the key stored in ai.json is the fallback.
    config.api_key = "sk-from-file".into();
    assert_eq!(config.api_key().as_deref(), Some("sk-from-file"));
    // The environment still wins over the stored key when both are present.
    std::env::set_var("ANTHROPIC_API_KEY", "sk-from-env");
    assert_eq!(config.api_key().as_deref(), Some("sk-from-env"));
    std::env::remove_var("ANTHROPIC_API_KEY");
}

#[test]
fn skill_parse_uses_first_heading_as_description() {
    use log_scouter::ai::skills::{list_in, parse_skill};

    let skill = parse_skill("triage", "# OOM triage\n\nLook at memory first.\n");
    assert_eq!(skill.name, "triage");
    assert_eq!(skill.description, "OOM triage");
    assert!(skill.body.contains("Look at memory first."));

    // A body with no heading falls back to the first non-empty line.
    let plain = parse_skill("notes", "\n\ncheck the disk\nthen the network\n");
    assert_eq!(plain.description, "check the disk");

    // list_in reads *.md from a directory, sorted, and ignores everything else.
    let dir = std::env::temp_dir().join(format!("logscout-skills-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("b.md"), "# Beta\nbody b").unwrap();
    std::fs::write(dir.join("a.md"), "# Alpha\nbody a").unwrap();
    std::fs::write(dir.join("ignore.txt"), "not a skill").unwrap();
    let found = list_in(&dir);
    assert_eq!(
        found.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert_eq!(found[0].description, "Alpha");
    let _ = std::fs::remove_dir_all(&dir);
}
