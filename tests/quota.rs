use llm_api::{Client, CodexQuota, Provider, Request, decode};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::TcpListener;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn http(provider: Provider, status: u16, body: &str) -> llm_api::Result<llm_api::Response> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
    let reply = format!(
        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nRetry-After: 73\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut socket);
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        reader.read_exact(&mut vec![0; length]).unwrap();
        socket.write_all(reply.as_bytes()).unwrap();
    });
    let client = if provider == Provider::Codex {
        Client::codex("synthetic-token", "synthetic-account").unwrap()
    } else {
        Client::new(provider, "synthetic-token")
    };
    let request: Request = serde_json::from_value(json!({
        "model":"test", "max_tokens":16, "messages":[]
    }))
    .unwrap();
    let result = client
        .with_endpoint(endpoint)
        .unwrap()
        .run(&request, |_| true);
    server.join().unwrap();
    result
}

#[test]
fn codex_quota_requires_exact_structured_http_429() {
    let confirmed = r#"{"error":{"type":"usage_limit_reached","message":"private test text"}}"#;
    let error = http(Provider::Codex, 429, confirmed).unwrap_err();
    assert_eq!(
        error.downcast_ref::<CodexQuota>().unwrap().reset_after(),
        Some(Duration::from_secs(73))
    );
    let oversized = format!("{confirmed}{}", " ".repeat(16 * 1024));
    for (provider, status, body) in [
        (Provider::Codex, 429, ""),
        (
            Provider::Codex,
            429,
            r#"{"error":{"code":"rate_limit_exceeded"}}"#,
        ),
        (
            Provider::Codex,
            429,
            r#"{"error":{"code":"insufficient_quota"}}"#,
        ),
        (
            Provider::Codex,
            429,
            r#"{"error":{"message":"usage_limit_reached"}}"#,
        ),
        (Provider::Codex, 429, &oversized),
        (Provider::Codex, 403, confirmed),
        (Provider::Openai, 429, confirmed),
    ] {
        let error = http(provider, status, body).unwrap_err();
        assert!(!error.is::<CodexQuota>(), "{error}");
        assert!(!error.to_string().contains("private test text"));
    }
}

fn sse(event: Value) -> Box<dyn std::error::Error + Send + Sync> {
    decode(
        Provider::Codex,
        "test",
        Cursor::new(format!("data: {event}\n\n")),
        |_| true,
    )
    .unwrap_err()
}

#[test]
fn quota_timing_precedence_and_sse_error_shapes() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let error = http(
        Provider::Codex,
        429,
        &json!({"error":{
            "code":"usage_limit_reached", "resets_at":now + 360, "resets_in_seconds":800
        }})
        .to_string(),
    )
    .unwrap_err();
    let seconds = error
        .downcast_ref::<CodexQuota>()
        .unwrap()
        .reset_after()
        .unwrap()
        .as_secs();
    assert!((350..=360).contains(&seconds));
    for (event, seconds) in [
        (
            json!({"type":"response.failed","response":{"error":{
                "code":"usage_limit_reached", "resets_at":1,"resets_in_seconds":42
            }}}),
            Some(42),
        ),
        (
            json!({"type":"error","error":{"type":"usage_limit_reached","resets_in_seconds":0}}),
            None,
        ),
        (json!({"type":"error","code":"usage_limit_reached"}), None),
    ] {
        let error = sse(event);
        assert_eq!(
            error.downcast_ref::<CodexQuota>().unwrap().reset_after(),
            seconds.map(Duration::from_secs)
        );
    }
    for code in [
        "rate_limit_exceeded",
        "insufficient_quota",
        "usage_limit_reached_other",
    ] {
        assert!(!sse(json!({"type":"error","code":code})).is::<CodexQuota>());
    }
}
