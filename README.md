# LLM API

Every agent application interacts with third-party LLMs over standard APIs. Services like litellm provide a basic compatibility layer for all these APIs. This repo is a minimal implementation of an API layer for the LLM services I care about, without the excess features present in other services. This repo is meant to be as small as possible and be updated rarely, to minimize surface area of security issues. It will only have minimal dependencies, such as those required for networking. This implementation should still be portable, and easily useable by most mainstream languages.

This repo will support the following LLM APIs:
- OpenRouter
- OpenAI
- Anthropic
- XAI
- Gemini

This repo will use AI in its creation and maintenance, but the intent is to be small enough a human can comfortably read all the code. Other public sources may be used as a reference. This should not be a complex problem to solve, and code from existing solutions may be incorporated (in a license-permitted way).

## Implementation

Blocking Rust library and JSON-lines executable. Supports text, inline base64 images, function tools/results, JSON-schema output requests, reasoning streams, token usage, and lossless native reasoning replay. OpenAI, xAI, and OpenRouter share the Responses protocol; Anthropic and Gemini have small native adapters.

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

`cargo test` uses fabricated SSE fixtures and loopback HTTP servers, requiring no keys or paid requests. Opt-in live checks are in `tests/live.py`. [Live results](docs/LIVE_TESTS.md) confirm text, JSON, tools/replay, images, and reasoning/replay on an OpenRouter free model; OpenAI generation was blocked by exhausted credits. Other providers and models remain live-unverified. Schema contents are forwarded, not rewritten or locally validated.

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

For an agent loop, append `response.message` intact to the history. Execute tools only when the finish reason is `tool_calls` and after your own authorization/schema checks. Append a user message with matching `tool_result` blocks, then call again. Preserve the replay payload; reasoning signatures and encrypted state cannot be reconstructed from visible reasoning text.

## Use from other languages

Launch `target/release/llm-api` and exchange UTF-8 JSON lines over stdin/stdout. Each command runs sequentially and emits deltas followed by `done` or `error`; flushes happen after each output line. Read incrementally rather than waiting for the process to exit.

```sh
export OPENAI_API_KEY='your-key'
printf '%s\n' '{"provider":"openai","request":{"model":"YOUR_MODEL_ID","messages":[{"role":"user","content":[{"type":"text","text":"Hello"}]}],"max_tokens":1024}}' | target/release/llm-api
```

Other keys are `ANTHROPIC_API_KEY`, `XAI_API_KEY`, `GEMINI_API_KEY`, and `OPENROUTER_API_KEY`. Keys never appear in command JSON. Do not expose endpoint overrides or arbitrary native options to untrusted callers when supplying your own credentials. HTTP errors omit response bodies, redirects are disabled, and each request has a 180-second timeout and a 16 MiB request/stream limit.
