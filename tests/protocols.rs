use llm_api::{Client, Message, Provider, Request, decode, encode};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn request() -> Request {
    serde_json::from_value(json!({
        "model":"test-model", "system":"Be precise", "max_tokens":2048,
        "messages":[{"role":"user","content":[{"type":"text","text":"Hello"}]}],
        "tools":[{"name":"weather","description":"Weather","parameters":{"type":"object","properties":{}}}],
        "json_schema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false}
    })).unwrap()
}

fn sse(events: &[Value]) -> String {
    events.iter().map(|e| format!("data: {e}\n\n")).collect()
}

fn responses() -> Vec<Value> {
    vec![
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"Consider"}),
        json!({"type":"response.output_text.delta","output_index":1,"delta":"Hello"}),
        json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","call_id":"c1","name":"weather"}}),
        json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"{}"}),
        json!({"type":"response.completed","response":{"output":[
            {"type":"reasoning","id":"r1","encrypted_content":"opaque","summary":[{"type":"summary_text","text":"Consider"}]},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello"}]},
            {"type":"function_call","id":"f1","call_id":"c1","name":"weather","arguments":"{}"}
        ],"usage":{"input_tokens":12,"output_tokens":9,"input_tokens_details":{"cached_tokens":2},"output_tokens_details":{"reasoning_tokens":4}}}}),
    ]
}

fn anthropic(stop: &str) -> Vec<Value> {
    vec![
        json!({"type":"message_start","message":{"id":"m1","type":"message","model":"test-model","role":"assistant","content":[],"usage":{"input_tokens":12,"output_tokens":1,"cache_read_input_tokens":2}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Consider"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signed"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"c1","name":"weather","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{"}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"}"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":9}}),
        json!({"type":"message_stop"}),
    ]
}

fn client(provider: Provider) -> Client {
    match provider {
        Provider::Codex => Client::codex("test-secret", "test-account").unwrap(),
        _ => Client::new(provider, "test-secret"),
    }
}

fn stream(provider: Provider) -> String {
    match provider {
        Provider::Openai | Provider::Codex | Provider::Openrouter => sse(&responses()),
        Provider::Anthropic => sse(&anthropic("tool_use")),
    }
}

#[test]
fn common_inputs_map_to_all_native_formats() {
    for provider in [
        Provider::Openai,
        Provider::Codex,
        Provider::Anthropic,
        Provider::Openrouter,
    ] {
        let mut req = request();
        req.messages[0]
            .content
            .push(json!({"type":"image","mime_type":"image/png","data":"aGVsbG8="}));
        let wire = encode(provider, &req).unwrap();
        match provider {
            Provider::Openai | Provider::Codex | Provider::Openrouter => {
                assert!(wire["input"].as_array().unwrap().iter().any(|item| {
                    item["content"].as_array().is_some_and(|content| {
                        content
                            .iter()
                            .any(|block| block["image_url"] == "data:image/png;base64,aGVsbG8=")
                    })
                }));
                assert_eq!(
                    wire["text"]["format"]["schema"],
                    req.json_schema.clone().unwrap()
                );
                assert_eq!(wire["tools"][0]["parameters"], req.tools[0]["parameters"]);
                assert_eq!(wire["store"], false);
            }
            Provider::Anthropic => {
                assert_eq!(
                    wire["messages"][0]["content"][1]["source"]["data"],
                    "aGVsbG8="
                );
                assert_eq!(
                    wire["output_config"]["format"]["schema"],
                    req.json_schema.clone().unwrap()
                );
                assert_eq!(wire["tools"][0]["input_schema"], req.tools[0]["parameters"]);
            }
        }
    }
}

#[test]
fn normalized_tools_usage_and_reasoning_round_trip() {
    for provider in [
        Provider::Openai,
        Provider::Codex,
        Provider::Anthropic,
        Provider::Openrouter,
    ] {
        let mut events = Vec::new();
        let result = decode(provider, "test-model", Cursor::new(stream(provider)), |e| {
            events.push(e);
            true
        })
        .unwrap();
        let call = result
            .message
            .content
            .iter()
            .find(|b| b["type"] == "tool_call")
            .unwrap();
        assert_eq!(call["name"], "weather");
        assert_eq!(call["arguments"], json!({}));
        assert!(
            result
                .message
                .content
                .iter()
                .any(|b| b["type"] == "reasoning")
        );
        assert_eq!(result.finish_reason, "tool_calls");
        assert_eq!(result.usage["normalized"]["input_tokens"], 12);
        assert_eq!(result.usage["normalized"]["output_tokens"], 9);
        assert!(events.iter().any(|e| e["type"] == "tool_call_start"));
        assert!(events.iter().any(|e| e["type"] == "tool_arguments_delta"));
        let mut req = request();
        req.messages.push(result.message.clone());
        req.messages.push(Message {
            role: "user".into(),
            content: vec![
                json!({"type":"tool_result","id":call["id"],"output":{"temperature":20}}),
            ],
            replay: None,
        });
        let wire = encode(provider, &req).unwrap();
        match provider {
            Provider::Openai | Provider::Codex | Provider::Openrouter => {
                assert!(wire["input"].as_array().unwrap().iter().any(|item| {
                    item["type"] == "reasoning" && item["encrypted_content"] == "opaque"
                }))
            }
            Provider::Anthropic => {
                assert_eq!(
                    wire["messages"][1],
                    json!({"role":"assistant","content":[{"type":"thinking","thinking":"Consider","signature":"signed"},{"type":"tool_use","id":"c1","name":"weather","input":{}}]})
                );
                assert_eq!(wire["messages"][2]["content"][0]["tool_use_id"], call["id"]);
            }
        }
        req.model = "other-model".into();
        assert!(encode(provider, &req).is_err());
    }
}

#[test]
fn truncation_and_safety_never_become_successful_tool_calls() {
    for reason in ["max_tokens", "refusal"] {
        let result = decode(
            Provider::Anthropic,
            "test-model",
            Cursor::new(sse(&anthropic(reason))),
            |_| true,
        )
        .unwrap();
        assert_eq!(
            result.finish_reason,
            if reason == "max_tokens" {
                "length"
            } else {
                reason
            }
        );
    }
}

#[test]
fn responses_preserve_mixed_block_order() {
    let mut req = request();
    req.messages = vec![Message {
        role: "assistant".into(),
        content: vec![
            json!({"type":"text","text":"before"}),
            json!({"type":"tool_call","id":"c","name":"weather","arguments":{}}),
            json!({"type":"text","text":"after"}),
        ],
        replay: None,
    }];
    for provider in [Provider::Openai, Provider::Codex] {
        let wire = encode(provider, &req).unwrap();
        assert_eq!(wire["input"][0]["content"][0]["text"], "before");
        assert_eq!(wire["input"][1]["type"], "function_call");
        assert_eq!(wire["input"][2]["content"][0]["text"], "after");
    }
}

#[test]
fn sse_supports_comments_crlf_bom_multiline_and_eof_dispatch() {
    let event = responses().pop().unwrap();
    let pretty = serde_json::to_string_pretty(&event)
        .unwrap()
        .lines()
        .map(|l| format!("data: {l}\r\n"))
        .collect::<String>();
    let wire = format!("\u{feff}: comment\r\nevent: ignored\r\nid: 1\r\n{pretty}\r\n");
    assert!(decode(Provider::Openai, "test-model", Cursor::new(wire), |_| true).is_ok());
    let eof = format!("data: {event}");
    assert!(decode(Provider::Openai, "test-model", Cursor::new(eof), |_| true).is_ok());
}

#[test]
fn malformed_truncated_and_error_streams_fail_closed() {
    for provider in [
        Provider::Openai,
        Provider::Codex,
        Provider::Anthropic,
        Provider::Openrouter,
    ] {
        assert!(decode(provider, "test-model", Cursor::new(""), |_| true).is_err());
        assert!(decode(provider, "test-model", Cursor::new("data: {\n\n"), |_| true).is_err());
        assert!(
            decode(
                provider,
                "test-model",
                Cursor::new(sse(&[json!({"error":{"message":"secret"}})])),
                |_| true
            )
            .unwrap_err()
            .to_string()
            .find("secret")
            .is_none()
        );
        let mut wire = stream(provider);
        wire.truncate(wire.find("\n\n").unwrap() + 2);
        assert!(decode(provider, "test-model", Cursor::new(wire), |_| true).is_err());
    }
    let mut events = responses();
    events.last_mut().unwrap()["response"]["output"][2]["arguments"] = json!("{");
    assert!(
        decode(
            Provider::Openai,
            "test-model",
            Cursor::new(sse(&events)),
            |_| true
        )
        .is_err()
    );
}

#[test]
fn callback_can_cancel() {
    for provider in [
        Provider::Openai,
        Provider::Codex,
        Provider::Anthropic,
        Provider::Openrouter,
    ] {
        let error = decode(
            provider,
            "test-model",
            Cursor::new(stream(provider)),
            |_| false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }
}

#[test]
fn invalid_inputs_and_endpoint_authorities_are_rejected() {
    for endpoint in [
        "http://example.com",
        "http://127.0.0.1:80@evil.example/",
        "http://[::1]:80@evil.example/",
        "https://user:password@example.com/",
        "ftp://example.com/",
    ] {
        assert!(
            Client::new(Provider::Openai, "secret")
                .with_endpoint(endpoint)
                .is_err(),
            "{endpoint}"
        );
    }
    assert!(
        Client::new(Provider::Openai, "secret")
            .with_endpoint("http://127.0.0.1:1234/responses")
            .is_ok()
    );
    let mut req = request();
    req.max_tokens = 0;
    assert!(encode(Provider::Openai, &req).is_err());
    req.max_tokens = 100;
    req.messages[0].content =
        vec![json!({"type":"tool_call","id":"c","name":"weather","arguments":{}})];
    assert!(encode(Provider::Openai, &req).is_err());
    req.messages[0].content = vec![json!({"type":"reasoning","text":"thought"})];
    assert!(encode(Provider::Anthropic, &req).is_err());
}

fn serve(body: String, status: &str) -> (String, thread::JoinHandle<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/endpoint", listener.local_addr().unwrap());
    let status = status.to_owned();
    let handle = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            headers.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        let length: usize = headers
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse().unwrap())
            })
            .unwrap();
        let mut input = vec![0; length];
        reader.read_exact(&mut input).unwrap();
        write!(socket,"HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        (headers, serde_json::from_slice(&input).unwrap())
    });
    (address, handle)
}

#[test]
fn actual_http_transport_for_every_provider() {
    for provider in [
        Provider::Openai,
        Provider::Codex,
        Provider::Anthropic,
        Provider::Openrouter,
    ] {
        let (endpoint, server) = serve(stream(provider), "200 OK");
        let mut req = request();
        if provider == Provider::Codex {
            req.options = serde_json::from_value(json!({
                "max_output_tokens":99, "store":true, "stream":false,
                "instructions":"", "tool_choice":"none", "parallel_tool_calls":false,
                "text":{"verbosity":"high"}
            }))
            .unwrap();
        }
        let response = client(provider)
            .with_endpoint(endpoint)
            .unwrap()
            .run(&req, |_| true)
            .unwrap();
        assert_eq!(response.finish_reason, "tool_calls");
        let (headers, body) = server.join().unwrap();
        let lower = headers.to_ascii_lowercase();
        match provider {
            Provider::Anthropic => {
                assert!(lower.contains("x-api-key: test-secret"));
                assert!(lower.contains("anthropic-version: 2023-06-01"));
            }
            Provider::Codex => {
                assert!(lower.contains("authorization: bearer test-secret"));
                assert!(lower.contains("chatgpt-account-id: test-account"));
                assert!(lower.contains("originator: llm-api"));
                assert!(lower.contains("openai-beta: responses=experimental"));
                assert!(lower.contains("accept: text/event-stream"));
                assert!(body.get("max_output_tokens").is_none());
                assert_eq!(body["store"], false);
                assert_eq!(body["stream"], true);
                assert_eq!(body["tool_choice"], "none");
                assert_eq!(body["parallel_tool_calls"], true);
                assert_eq!(body["text"]["verbosity"], "high");
                assert!(!body["instructions"].as_str().unwrap().trim().is_empty());
            }
            _ => assert!(lower.contains("authorization: bearer test-secret")),
        }
        assert!(!body.to_string().contains("test-secret"));
        assert!(!body.to_string().contains("test-account"));
    }
}

#[test]
fn http_errors_omit_sensitive_bodies() {
    let (endpoint, server) = serve("provider leaked test-secret".into(), "401 Unauthorized");
    let error = Client::new(Provider::Openai, "test-secret")
        .with_endpoint(endpoint)
        .unwrap()
        .run(&request(), |_| true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("401"));
    assert!(!error.contains("test-secret"));
    server.join().unwrap();
}

#[test]
fn cli_reports_errors_then_processes_next_request() {
    let (endpoint, server) = serve(stream(Provider::Openai), "200 OK");
    let mut child = Command::new(env!("CARGO_BIN_EXE_llm-api"))
        .env("OPENAI_API_KEY", "test-secret")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "not-json").unwrap();
    writeln!(
        input,
        "{}",
        json!({"provider":"openai","endpoint":endpoint,"request":request()})
    )
    .unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events[0]["type"], "error");
    assert_eq!(events.last().unwrap()["type"], "done");
    server.join().unwrap();
}

#[test]
fn streaming_callback_happens_before_terminal_response_arrives() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
    let (send, receive) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
            if line == "\r\n" {
                break;
            }
        }
        reader.read_exact(&mut vec![0; length]).unwrap();
        let events = responses();
        let first = sse(&events[..1]);
        let last = sse(&events[1..]);
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{first}",first.len()+last.len()).unwrap();
        socket.flush().unwrap();
        receive.recv_timeout(Duration::from_secs(5)).unwrap();
        socket.write_all(last.as_bytes()).unwrap();
    });
    Client::new(Provider::Openai, "secret")
        .with_endpoint(endpoint)
        .unwrap()
        .run(&request(), |_| {
            send.send(()).ok();
            true
        })
        .unwrap();
    server.join().unwrap();
}
