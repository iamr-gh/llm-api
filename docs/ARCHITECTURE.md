# Architecture

A small blocking Rust client plus a JSONL subprocess interface. It translates one request shape into three wire protocols: OpenAI-compatible Responses (OpenAI, xAI, and OpenRouter), Anthropic Messages, and Gemini streamGenerateContent, consumes SSE, emits incremental events, and returns an assistant message with native replay data. It does not run tools, fetch images, manage conversations, or validate generated JSON against a schema.

## Public surface

Defined in `src/lib.rs`:

- `Provider`: `openai`, `xai`, `openrouter`, `anthropic`, `gemini` (serialized names).
- `Client::new(provider, key)`: select the default endpoint and retain caller-supplied credentials.
- `Client::with_endpoint(endpoint) -> Result<Self>`: override the endpoint; HTTPS is required except for HTTP on `127.0.0.1:` or `[::1]:`.
- `Client::run(&Request, emit: impl FnMut(Value) -> bool) -> Result<Response>`: encode, POST, decode SSE, then return the final response. Returning `false` from the callback cancels with an error.
- `encode(Provider, &Request) -> Result<Value>`: build the native JSON body without networking.
- `decode(Provider, model, impl BufRead, emit) -> Result<Response>`: consume an SSE reader without making a request.
- `Request`, `Message`, `Response`, `Result<T>` and `LIMIT` (16 MiB) are public. Errors are boxed, thread-safe standard errors.

See [FORMATS.md](FORMATS.md) for fields, events, native examples, and replay.

| Provider | Default target | Authentication |
| --- | --- | --- |
| OpenAI | `https://api.openai.com/v1/responses` | Bearer token |
| xAI | `https://api.x.ai/v1/responses` | Bearer token |
| OpenRouter | `https://openrouter.ai/api/v1/responses` | Bearer token |
| Anthropic | `https://api.anthropic.com/v1/messages` | `x-api-key`; `anthropic-version: 2023-06-01` |
| Gemini | `https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent?alt=sse` | `x-goog-api-key` |

OpenRouter uses stateless Responses, not server-side conversation storage. The shared Responses encoder forces `store: false` and requests `include: ["reasoning.encrypted_content"]`; callers supply full history with native output replay. The [official OpenRouter Responses overview](https://openrouter.ai/docs/api_reference/responses/overview.md) documents encrypted reasoning and tool calling, and rejects `store: true` or non-null `previous_response_id`. Use `store: false` and omit `previous_response_id`; the client does not remove a caller-supplied ID from native options.

Anthropic replay retains only `role: "assistant"` and assembled `content`, stripping response-only envelope fields while preserving thinking signatures and redacted blocks. Gemini tool-result names are resolved from preceding normalized tool calls, including calls in replay-bearing assistant messages; retain normalized content alongside replay.

For Gemini, an endpoint override replaces the models base URL; the model and operation suffix are still appended. Model IDs may contain only ASCII letters, digits, `-`, `.`, and `_`.

## Data flow and boundaries

1. `encode` starts with the provider-native `options` object. Standard fields overwrite their corresponding native fields; nested settings such as `generationConfig` are merged where implemented. This is not a general options validator or portable reasoning abstraction.
2. Messages become native text, inline images, tool calls/results, or native replay preserving protocol-specific content. Tool declarations and output schemas are wrapped for the selected protocol; their schema subsets are not transformed to fit provider capabilities.
3. `run` serializes and bounds the request, sends one POST, checks HTTP success, and reads SSE. There is no application retry loop. Redirects are disabled; the global request timeout is 180 seconds.
4. `decode` reads `data:` records, joining multiline data and ignoring other SSE fields. Framing accepts LF and CRLF, including an initial UTF-8 BOM; standalone-CR framing is not supported. It handles provider errors and terminal markers, emits normalized deltas, and assembles native output. OpenAI, xAI, and OpenRouter share the Responses encoder/decoder; Gemini uses only the first candidate.
5. Finalization parses tool argument JSON, builds normalized content and usage, and attaches provider/model-bound native replay. The caller appends the returned assistant message and any tool results to the next request.

An event callback runs only when a normalized event is emitted. It is not a background cancellation token: an idle network read cannot be interrupted through it, and is bounded by the request timeout. Callback cancellation returns an error, not a partial `Response`.

Requests serialized by `run` and SSE bytes read by `decode` each have a 16 MiB limit. This is a wire/input bound, not a guarantee that all in-memory representations together fit in 16 MiB. Missing terminal events are errors. Incomplete or malformed tool argument JSON is also an error, even after deltas have been delivered and even on a length-limited generation. A successful `length` response may still contain partial text or syntactically valid but semantically incomplete arguments; callers must decide whether to use them.

## Security and ownership

The caller owns endpoint trust, credentials, model selection, native options, history, and tool authorization. HTTPS alone does not make an endpoint trustworthy: an override receives the key and conversation. A service exposing its own credentials must not let untrusted callers control endpoints or arbitrary native options; constrain these at the service boundary, along with models and other resource controls.

- No redirects, tool execution, image URL fetching, or automatic retries are implemented.
- Images contain caller-supplied base64 strings and MIME types. Only the `image/` MIME prefix and string fields are checked; image bytes/base64 are not decoded or inspected.
- Tool schemas and output schemas are forwarded, not locally enforced. Generated JSON remains text, not a locally parsed/schema-validated result. Tool argument JSON is parsed and required to be an object, but is not checked against the tool's parameter schema.
- Treat text, reasoning, tool arguments/results, and replay as sensitive, untrusted data. Preserve replay without rewriting or executing it. Replay is not cross-provider or cross-model portable.
- HTTP error bodies and provider stream-error payloads are omitted from errors to reduce secret exposure. This is not a blanket sanitization guarantee for all errors or application logs.

These documents describe local implementation behavior. [Live smoke results](LIVE_TESTS.md) cover one OpenRouter free model; OpenAI generation was blocked by exhausted credits. Other provider/model combinations remain live-unverified.

## Subprocess portability

`src/main.rs` exposes the same blocking client to languages that can spawn a process and exchange UTF-8 JSON lines. Build with `cargo build --release`; the executable is `target/release/llm-api`. These are usage commands, not verification results.

Send one command object per stdin line:

```json
{"provider":"openai","request":{"model":"MODEL_ID","messages":[{"role":"user","content":[{"type":"text","text":"Hello"}]}],"max_tokens":256}}
```

An optional command-level `endpoint` overrides the target. Keys are read from the environment, never from the command:

| Provider | Variable |
| --- | --- |
| OpenAI | `OPENAI_API_KEY` |
| xAI | `XAI_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Anthropic | `ANTHROPIC_API_KEY` |
| Gemini | `GEMINI_API_KEY` |

Stdout contains flushed JSONL events. For each nonblank, ordinary-sized input line, processing is sequential: zero or more deltas followed by either `{"type":"done","response":...}` or `{"type":"error","message":"..."}`. There are no request IDs and no interleaving. The library itself returns `Response`; only the CLI wraps it in `done`.

Malformed JSON, unknown command/request/message fields, unsupported provider names, missing keys, encoding failures, and provider failures produce an `error` line, then the process handles the next command. An error may follow already-emitted deltas; it has no `done`. Blank lines are ignored. A command line over 16 MiB, input read/UTF-8 failure, or unrecoverable stdout failure terminates the process instead of guaranteeing a per-line error event. Applications must handle EOF/process failure as well as JSONL errors. Do not assume exit success means every command succeeded.
