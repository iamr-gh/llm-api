# LLM API

Every agent application interacts with third-party LLMs over standard APIs. Services like litellm provide a basic compatibility layer for all these APIs. This repo is a minimal implementation of an API layer for the LLM services I care about, without the excess features present in other services. This repo is meant to be as small as possible and be updated rarely, to minimize surface area of security issues. It will only have minimal dependencies, such as those required for networking. This implementation should still be portable, and easily useable by most mainstream languages.

This repo will support the following LLM APIs:
- OpenRouter
- OpenAI
- Anthropic

This repo will use AI in its creation and maintenance, but the intent is to be small enough a human can comfortably read all the code. Other public sources may be used as a reference. This should not be a complex problem to solve, and code from existing solutions may be incorporated (in a license-permitted way).

## Implementation

Blocking Rust library and JSON-lines executable for OpenAI, Anthropic, and OpenRouter API keys, plus a separate Codex subscription mode. Supports text, inline base64 images, function tools/results, JSON-schema output requests, reasoning streams, token usage, and native reasoning replay. OpenAI, OpenRouter, and Codex share Responses; Anthropic uses Messages. No other providers are implemented.

Three direct dependencies: `serde`, `serde_json`, and `ureq` with Rustls TLS. Networking/TLS and serialization have transitive dependencies; this is not a dependency-free or 700-line-total supply chain. `Cargo.lock` records the resolved versions. There are no provider SDKs, async runtime, gateway, retry loop, pricing catalog, or tool execution engine.

Read [Architecture](docs/ARCHITECTURE.md) for boundaries and security, and [Formats](docs/FORMATS.md) for each protocol, reasoning controls, replay, and tool round trips. Production Rust is in `src/lib.rs` and `src/main.rs`; tests and documentation are separate.

## Build and check

Requires Rust 1.85 or newer.

```sh
cargo build --release --locked
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
wc -l src/*.rs
```

`cargo test` uses fabricated SSE fixtures and loopback HTTP servers, requiring no keys or paid requests. Opt-in checks in `tests/live.py` have exercised text, JSON, tools/replay, images, and native reasoning/replay on one model in each mode: OpenAI, Anthropic, OpenRouter free, and Codex using the existing fx subscription login. See [live results](docs/LIVE_TESTS.md) for models, commands, token usage, and test limitations. Schema contents are forwarded, not rewritten or locally validated.

## Use from Rust

Add this repository as a path dependency, then deserialize the small common request format or construct `Request` and `Message` directly:

```rust
use llm_api::{Client, Provider, Request};
use serde_json::json;

fn main() -> llm_api::Result<()> {
    let request: Request = serde_json::from_value(json!({
        "model": std::env::var("MODEL")?,
        "messages": [{"role": "user", "content": [{"type": "text", "text": "Hello"}]}],
        "max_tokens": 1024
    }))?;
    let client = Client::new(Provider::Openai, std::env::var("OPENAI_API_KEY")?);
    let response = client.run(&request, |event| {
        eprintln!("{event}");
        true
    })?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}
```

### Cancellation

`Client::run_cancellable(&request, &cancelled, emit)` accepts a standard `AtomicBool`. Store `true` from another thread or a signal-safe handler and keep it set until the call returns. A scoped monitor checks every 10 ms and shuts down that request's sockets, including idle response reads and blocked TLS handshakes. The request and callback still run on the calling thread; the callback does not need `Send` or `'static`. Callbacks must themselves return promptly.

```rust
use std::sync::atomic::{AtomicBool, Ordering};

let cancelled = AtomicBool::new(false);
// Another thread may call cancelled.store(true, Ordering::Relaxed).
let result = client.run_cancellable(&request, &cancelled, |event| {
    eprintln!("{event}");
    true
});
if let Err(error) = &result {
    if error.is::<llm_api::Cancelled>() {
        // Discard partial deltas; no final response or executable tool calls returned.
    }
}
```

Cancellation has a typed `Cancelled` error, including callback cancellation. Each cancellable call gets an isolated transport; cancelling one does not poison the client or cancel another. It does not reuse pooled connections between calls. No request worker is detached, and the monitor is joined on return or callback panic. Cancellation cannot retract a request already sent, stop provider billing, undo external effects, or preempt a blocked application callback.

DNS lookup and TCP connection setup cannot be interrupted by socket shutdown; each stage has a five-second timeout (all calls retain the 180-second global timeout). A system DNS lookup may outlive its timed-out waiter inside `ureq`. Normal `Client::run` remains available with pooled transport and callback-only cancellation. This API requires `ureq` 3.5.1 or newer; no dependency was added. The sequential JSONL executable has no in-band cancel command; this new API is currently available to Rust callers.

For an agent loop, append `response.message` intact to the history. Execute tools only when the finish reason is `tool_calls` and after your own authorization/schema checks. Append a user message with matching `tool_result` blocks, then call again. Preserve the replay payload; reasoning signatures and encrypted state cannot be reconstructed from visible reasoning text.

## Use from other languages

Launch `target/release/llm-api` and exchange UTF-8 JSON lines over stdin/stdout. Each command runs sequentially and emits deltas followed by `done` or `error`; flushes happen after each output line. Read incrementally rather than waiting for the process to exit.

```sh
export OPENAI_API_KEY='your-key'
printf '%s\n' '{"provider":"openai","request":{"model":"YOUR_MODEL_ID","messages":[{"role":"user","content":[{"type":"text","text":"Hello"}]}],"max_tokens":1024}}' | target/release/llm-api
```

The other API keys are `ANTHROPIC_API_KEY` and `OPENROUTER_API_KEY`. Keys never appear in command JSON. Do not expose endpoint overrides or arbitrary native options to untrusted callers when supplying your own credentials. HTTP errors omit response bodies, redirects are disabled, and each request has a 180-second timeout and a 16 MiB request/stream limit.

## Codex subscription

Subscription access is distinct from an OpenAI API key. In Rust, construct `Client::codex(access_token, account_id)?`; in the CLI, select `"provider":"codex"` and supply `CODEX_ACCESS_TOKEN` and `CODEX_ACCOUNT_ID` in the process environment. The caller owns login, token expiry, and refresh. The crate never reads or writes an fx profile and never receives a refresh token.

For the explicit live test, `tests/live.py --provider codex --model YOUR_CODEX_MODEL_ID` uses `tests/codex_session.py` to read the existing fx login without modifying it or printing credentials. The bridge rejects near-expired tokens; refresh through fx before retrying. No subscription credentials are copied into this repository or `.env`.

Codex rejects the public API's output-token cap, so this mode omits `max_output_tokens`. The common positive `max_tokens` field remains required but does not cap subscription generation. Use short prompts, low reasoning effort, cancellation, and time/resource limits as appropriate. See [Formats](docs/FORMATS.md) for the exact subscription wire contract.
