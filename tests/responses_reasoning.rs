use llm_api::{Provider, decode};
use serde_json::{Value, json};
use std::io::Cursor;

#[test]
fn responses_reasoning_content_never_pollutes_structured_answer_text() {
    let events = [
        json!({"type":"response.reasoning_text.delta","output_index":0,"delta":"Private working"}),
        json!({"type":"response.output_text.delta","output_index":1,"delta":"{\"ok\":true}"}),
        json!({"type":"response.completed","response":{"output":[
            {"type":"reasoning","id":"r1","content":[{"type":"reasoning_text","text":"Private working"}],"encrypted_content":"opaque"},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"{\"ok\":true}"}]}
        ]}}),
    ];
    let wire: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
    for provider in [Provider::Openai, Provider::Codex, Provider::Openrouter] {
        let mut deltas = Vec::new();
        let response = decode(provider, "test", Cursor::new(&wire), |event| {
            deltas.push(event);
            true
        })
        .unwrap();
        let text: String = response
            .message
            .content
            .iter()
            .filter(|b| b["type"] == "text")
            .map(|b| b["text"].as_str().unwrap())
            .collect();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({"ok":true})
        );
        assert!(
            response
                .message
                .content
                .iter()
                .any(|b| b["type"] == "reasoning" && b["text"] == "Private working")
        );
        assert!(
            deltas
                .iter()
                .any(|e| e["type"] == "reasoning_delta" && e["delta"] == "Private working")
        );
        assert_eq!(
            response.message.replay.unwrap()["data"][0]["encrypted_content"],
            "opaque"
        );
    }
}
