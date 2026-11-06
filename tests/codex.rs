use llm_api::{Client, Provider, Request, decode, encode};
use serde_json::{Value, json};
use std::io::Cursor;
use std::net::TcpListener;

fn request(options: Value) -> Request {
    serde_json::from_value(json!({
        "model":"test-model", "max_tokens":2048,
        "messages":[{"role":"user","content":[{"type":"text","text":"Hello"}]}],
        "options":options
    }))
    .unwrap()
}

fn parse(events: &[Value]) -> llm_api::Result<llm_api::Response> {
    let wire: String = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect();
    decode(Provider::Codex, "test-model", Cursor::new(wire), |_| true)
}

fn message(text: &str) -> Value {
    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]})
}

#[test]
fn codex_forces_subscription_request_fields_and_never_sends_an_output_cap() {
    for system in ["", "   ", "Be precise"] {
        let mut req = request(json!({
            "max_output_tokens":17, "store":true, "stream":false,
            "instructions":"", "tool_choice":"none", "parallel_tool_calls":false,
            "text":{"verbosity":"high"}, "reasoning":{"effort":"high"}
        }));
        req.system = system.into();
        let wire = encode(Provider::Codex, &req).unwrap();
        assert!(wire.get("max_output_tokens").is_none());
        assert!(wire.get("max_tokens").is_none());
        assert_eq!(wire["store"], false);
        assert_eq!(wire["stream"], true);
        assert_eq!(wire["tool_choice"], "none");
        assert_eq!(wire["parallel_tool_calls"], true);
        assert_eq!(wire["text"]["verbosity"], "high");
        assert_eq!(wire["reasoning"]["effort"], "high");
        assert!(!wire["instructions"].as_str().unwrap().trim().is_empty());
        if system == "Be precise" {
            assert_eq!(wire["instructions"], system);
        }
        assert!(wire["input"].is_array());
    }
    let req = request(json!({}));
    let wire = encode(Provider::Codex, &req).unwrap();
    assert!(wire.get("max_output_tokens").is_none());
    assert_eq!(wire["text"]["verbosity"], "low");
    assert_eq!(wire["tool_choice"], "auto");
    for provider in [Provider::Openai, Provider::Openrouter] {
        assert_eq!(encode(provider, &req).unwrap()["max_output_tokens"], 2048);
    }
}

#[test]
fn codex_still_requires_the_common_request_token_budget() {
    let mut req = request(json!({}));
    req.max_tokens = 0;
    assert!(encode(Provider::Codex, &req).is_err());
    let mut raw = serde_json::to_value(request(json!({}))).unwrap();
    raw.as_object_mut().unwrap().remove("max_tokens");
    assert!(serde_json::from_value::<Request>(raw).is_err());
}

#[test]
fn codex_constructor_rejects_empty_or_header_unsafe_account_ids() {
    for account in [
        "",
        " ",
        "\t",
        "bad\r\ninjected: value",
        "bad\nvalue",
        "bad\0value",
    ] {
        assert!(
            Client::codex("dummy-access-token", account).is_err(),
            "{account:?}"
        );
    }
    assert!(Client::codex("dummy-access-token", "dummy-account-id").is_ok());
}

#[test]
fn codex_without_an_account_fails_before_sending_http() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
    let result = Client::new(Provider::Codex, "dummy-access-token")
        .with_endpoint(endpoint)
        .unwrap()
        .run(&request(json!({})), |_| true);
    assert!(result.is_err());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn response_done_accepts_completed_and_incomplete_statuses() {
    for (status, reason) in [("completed", "stop"), ("incomplete", "length")] {
        let result = parse(&[json!({"type":"response.done","response":{
            "status":status, "output":[message("answer")],
            "incomplete_details":{"reason":"max_output_tokens"},
            "usage":{"input_tokens":3,"output_tokens":2}
        }})])
        .unwrap();
        assert_eq!(result.finish_reason, reason);
        assert_eq!(result.message.content[0]["text"], "answer");
        assert_eq!(result.usage["normalized"]["input_tokens"], 3);
    }
    let inferred = parse(&[json!({"type":"response.done","response":{
        "output":[message("partial")],"incomplete_details":{"reason":"max_output_tokens"}
    }})])
    .unwrap();
    assert_eq!(inferred.finish_reason, "length");
}

#[test]
fn unsuccessful_and_conflicting_terminal_statuses_fail_closed() {
    for event_type in ["response.done", "response.completed", "response.incomplete"] {
        for status in ["failed", "cancelled", "in_progress"] {
            assert!(
                parse(&[json!({"type":event_type,"response":{
                    "status":status,"output":[message("not success")]
                }})])
                .is_err(),
                "{event_type}: {status}"
            );
        }
    }
    for (event_type, status) in [
        ("response.completed", "incomplete"),
        ("response.incomplete", "completed"),
    ] {
        assert!(
            parse(&[json!({"type":event_type,"response":{
                "status":status,"output":[message("not success")]
            }})])
            .is_err(),
            "{event_type}: {status}"
        );
    }
}

#[test]
fn missing_or_null_terminal_output_recovers_full_done_items_in_index_order() {
    for null_output in [false, true] {
        for status in ["completed", "incomplete"] {
            let mut terminal = json!({"type":"response.done","response":{
                "status":status,"incomplete_details":{"reason":"content_filter"}
            }});
            if null_output {
                terminal["response"]["output"] = Value::Null;
            }
            let result = parse(&[
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"r1"}}),
                json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"m1"}}),
                json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","call_id":"c1","name":"weather"}}),
                json!({"type":"response.output_text.delta","output_index":1,"delta":"partial"}),
                json!({"type":"response.output_item.done","output_index":2,"item":{"type":"function_call","call_id":"c1","name":"weather","arguments":"{\"city\":\"Paris\"}"}}),
                json!({"type":"response.output_item.done","output_index":1,"item":message("complete answer")}),
                json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"r1","encrypted_content":"opaque","summary":[{"type":"summary_text","text":"Consider"}]}}),
                terminal,
            ]).unwrap();
            assert_eq!(
                result.finish_reason,
                if status == "completed" {
                    "tool_calls"
                } else {
                    "content_filter"
                }
            );
            assert!(
                result
                    .message
                    .content
                    .iter()
                    .any(|block| block["type"] == "text" && block["text"] == "complete answer")
            );
            assert!(
                result
                    .message
                    .content
                    .iter()
                    .any(|block| block["type"] == "tool_call"
                        && block["arguments"] == json!({"city":"Paris"}))
            );
            let replay = result.message.replay.unwrap();
            assert_eq!(replay["data"][0]["encrypted_content"], "opaque");
            assert_eq!(replay["data"][1], message("complete answer"));
            assert_eq!(replay["data"][2]["call_id"], "c1");
        }
    }
}

#[test]
fn pending_items_and_deltas_cannot_substitute_for_final_items() {
    for null_output in [false, true] {
        let mut terminal = json!({"type":"response.done","response":{"status":"completed"}});
        if null_output {
            terminal["response"]["output"] = Value::Null;
        }
        for item in [
            message("already looks complete"),
            json!({"type":"function_call","call_id":"c1","name":"weather","arguments":"{}"}),
        ] {
            assert!(
                parse(&[
                    json!({"type":"response.output_item.added","output_index":0,"item":item}),
                    json!({"type":"response.output_text.delta","output_index":0,"delta":"partial"}),
                    terminal.clone(),
                ])
                .is_err()
            );
        }
        assert!(parse(&[
            json!({"type":"response.output_item.done","output_index":0,"item":message("finished")}),
            json!({"type":"response.output_item.added","output_index":1,"item":message("pending")}),
            terminal,
        ]).is_err());
    }
}

#[test]
fn authoritative_terminal_output_can_complete_pending_items() {
    let result = parse(&[
        json!({"type":"response.output_item.added","output_index":0,"item":message("")}),
        json!({"type":"response.done","response":{"status":"completed","output":[message("final")]}}),
    ]).unwrap();
    assert_eq!(result.message.content[0]["text"], "final");
}

#[test]
fn output_item_indexes_are_bounded_including_added_and_done_events() {
    for event_type in [
        "response.output_item.added",
        "response.output_item.done",
        "response.output_text.delta",
        "response.function_call_arguments.delta",
    ] {
        for index in [json!(4097_u64), json!(u64::MAX), json!(-1), json!("0")] {
            assert!(parse(&[
                json!({"type":event_type,"output_index":index,"item":message("bad index"),"delta":"x"}),
                json!({"type":"response.done","response":{"status":"completed","output":[message("final")]}}),
            ]).is_err(), "{event_type}: {index}");
        }
    }
    let result = parse(&[
        json!({"type":"response.output_item.added","output_index":4096,"item":message("")}),
        json!({"type":"response.output_item.done","output_index":4096,"item":message("boundary")}),
        json!({"type":"response.done","response":{"status":"completed"}}),
    ])
    .unwrap();
    assert_eq!(result.message.content[0]["text"], "boundary");
}
