use llm_api::{Client, LIMIT, Provider, Request, decode, encode};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::TcpListener;
use std::thread;

fn request(options: Value) -> Request {
    serde_json::from_value(json!({"model":"test","messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}],"max_tokens":2048,"options":options})).unwrap()
}

fn sse(events: &[Value]) -> String {
    events.iter().map(|e| format!("data: {e}\n\n")).collect()
}

#[test]
fn reasoning_options_are_preserved_and_core_fields_cannot_be_overridden() {
    for provider in [Provider::Openai, Provider::Xai, Provider::Openrouter] {
        let req = request(
            json!({"reasoning":{"effort":"high","summary":"auto"},"model":"wrong","stream":false,"store":true}),
        );
        let wire = encode(provider, &req).unwrap();
        assert_eq!(wire["reasoning"], req.options["reasoning"]);
        assert_eq!(wire["model"], "test");
        assert_eq!(wire["stream"], true);
        assert_eq!(wire["store"], false);
        assert!(wire["input"].is_array());
    }
    let req = request(
        json!({"thinking":{"type":"enabled","budget_tokens":1024},"output_config":{"effort":"high"}}),
    );
    let wire = encode(Provider::Anthropic, &req).unwrap();
    assert_eq!(wire["thinking"], req.options["thinking"]);
    assert_eq!(wire["output_config"]["effort"], "high");
    let req = request(
        json!({"generationConfig":{"thinkingConfig":{"thinkingBudget":1024,"includeThoughts":true},"temperature":0.5}}),
    );
    let wire = encode(Provider::Gemini, &req).unwrap();
    assert_eq!(
        wire["generationConfig"]["thinkingConfig"],
        req.options["generationConfig"]["thinkingConfig"]
    );
    assert_eq!(wire["generationConfig"]["maxOutputTokens"], 2048);
}

#[test]
fn malformed_anthropic_blocks_and_missing_starts_return_errors() {
    for (message, next) in [
        (
            json!(7),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        ),
        (
            json!({"content":[7]}),
            json!({"type":"content_block_delta","index":0,"delta":{"text":"bad"}}),
        ),
    ] {
        let malformed = sse(&[json!({"type":"message_start","message":message}), next]);
        assert!(
            decode(Provider::Anthropic, "test", Cursor::new(malformed), |_| {
                true
            })
            .is_err()
        );
    }
    let malformed = sse(&[
        json!({"type":"message_start","message":{"content":[],"usage":{}}}),
        json!({"type":"content_block_start","index":0,"content_block":7}),
        json!({"type":"content_block_delta","index":0,"delta":{"text":"bad"}}),
    ]);
    assert!(
        decode(Provider::Anthropic, "test", Cursor::new(malformed), |_| {
            true
        })
        .is_err()
    );
    let missing = sse(&[json!({"type":"content_block_delta","index":0,"delta":{"text":"bad"}})]);
    assert!(decode(Provider::Anthropic, "test", Cursor::new(missing), |_| true).is_err());
    let huge = sse(&[json!({"type":"content_block_start","index":999999999,"content_block":{}})]);
    assert!(decode(Provider::Anthropic, "test", Cursor::new(huge), |_| true).is_err());
}

#[test]
fn incomplete_responses_preserve_the_actual_reason() {
    for reason in ["max_output_tokens", "content_filter"] {
        let wire = sse(&[
            json!({"type":"response.incomplete","response":{"output":[{"type":"message","content":[{"type":"output_text","text":"partial"}]}],"incomplete_details":{"reason":reason}}}),
        ]);
        let result = decode(Provider::Openai, "test", Cursor::new(wire), |_| true).unwrap();
        assert_eq!(
            result.finish_reason,
            if reason == "max_output_tokens" {
                "length"
            } else {
                reason
            }
        );
        assert_eq!(result.message.content[0]["text"], "partial");
        assert!(result.usage["normalized"]["input_tokens"].is_null());
    }
}

#[test]
fn parallel_tools_and_redacted_thinking_survive_anthropic_replay() {
    let wire = sse(&[
        json!({"type":"message_start","message":{"role":"assistant","content":[],"usage":{}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"opaque"}}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"a","name":"first","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"partial_json":"{\"x\":"}}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"b","name":"second","input":{}}}),
        json!({"type":"content_block_delta","index":2,"delta":{"partial_json":"{\"y\":2}"}}),
        json!({"type":"content_block_delta","index":1,"delta":{"partial_json":"1}"}}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
        json!({"type":"message_stop"}),
    ]);
    let response = decode(Provider::Anthropic, "test", Cursor::new(wire), |_| true).unwrap();
    assert_eq!(response.message.content[0]["arguments"], json!({"x":1}));
    assert_eq!(response.message.content[1]["arguments"], json!({"y":2}));
    let mut req = request(json!({}));
    req.messages.push(response.message);
    let encoded = encode(Provider::Anthropic, &req).unwrap();
    assert_eq!(
        encoded["messages"][1]["content"][0],
        json!({"type":"redacted_thinking","data":"opaque"})
    );
}

#[test]
fn gemini_keeps_usage_after_finish_and_native_signatures() {
    let wire = sse(&[
        json!({"candidates":[{"content":{"parts":[{"text":"hello","thoughtSignature":"opaque"}]},"finishReason":"STOP"}]}),
        json!({"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2}}),
    ]);
    let response = decode(Provider::Gemini, "test", Cursor::new(wire), |_| true).unwrap();
    assert_eq!(response.usage["normalized"]["input_tokens"], 10);
    assert_eq!(
        response.message.replay.unwrap()["data"]["parts"][0]["thoughtSignature"],
        "opaque"
    );
}

#[test]
fn single_byte_reads_bom_and_utf8_do_not_corrupt_events() {
    let wire = format!(
        "\u{feff}{}",
        sse(&[
            json!({"type":"response.output_text.delta","delta":"こんにちは 🦀"}),
            json!({"type":"response.completed","response":{"output":[{"type":"message","content":[{"type":"output_text","text":"こんにちは 🦀"}]}]}}),
        ])
    );
    let mut events = Vec::new();
    let response = decode(
        Provider::Openai,
        "test",
        BufReader::with_capacity(1, Cursor::new(wire)),
        |e| {
            events.push(e);
            true
        },
    )
    .unwrap();
    assert_eq!(events[0]["delta"], "こんにちは 🦀");
    assert_eq!(response.message.content[0]["text"], "こんにちは 🦀");
}

#[test]
fn oversized_stream_is_rejected() {
    let input = std::io::repeat(b'x').take((LIMIT + 1) as u64);
    let error = decode(Provider::Openai, "test", BufReader::new(input), |_| true).unwrap_err();
    assert!(error.to_string().contains("16 MiB"));
}

#[test]
fn redirects_are_never_followed() {
    let source = TcpListener::bind("127.0.0.1:0").unwrap();
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    destination.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/responses", source.local_addr().unwrap());
    let location = format!("http://{}/stolen", destination.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut socket, _) = source.accept().unwrap();
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
        write!(socket,"HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    assert!(
        Client::new(Provider::Openai, "secret")
            .with_endpoint(endpoint)
            .unwrap()
            .run(&request(json!({})), |_| true)
            .is_err()
    );
    server.join().unwrap();
    assert_eq!(
        destination.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
