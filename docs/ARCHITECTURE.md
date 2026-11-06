# Architecture

A small blocking Rust client plus a JSONL subprocess interface. It translates one request shape into two wire protocols: Responses (OpenAI and OpenRouter API keys, plus Codex subscription mode) and Anthropic Messages (API key), consumes SSE, emits incremental events, and returns an assistant message with native replay data. It does not run tools, fetch images, manage conversations, or validate generated JSON against a schema.

## Public surface

Defined in `src/lib.rs`:

- `Provider`: `openai`, `anthropic`, `openrouter`, `codex` (serialized names).
- `Client::new(provider, key)`: select the default endpoint and retain a caller-supplied API key.
- `Client::codex(access_token, account_id) -> Result<Self>`: construct a subscription client with a nonempty access token and nonempty printable ASCII account ID (at most 1024 bytes). `Client::new(Provider::Codex, token)` lacks the account ID and cannot run subscription requests.
- `Client::with_endpoint(endpoint) -> Result<Self>`: override the endpoint; HTTPS is required except for HTTP on `127.0.0.1` or `[::1]`; URL userinfo is rejected.
- `Client::run(&Request, emit: impl FnMut(Value) -> bool) -> Result<Response>`: encode, POST, decode SSE, then return the final response. Returning `false` from the callback returns typed `Cancelled`.
- `Client::run_cancellable(&Request, &AtomicBool, emit) -> Result<Response>`: same caller-thread callback and request format, with a request-isolated transport and scoped 10 ms cancellation monitor. An atomic flag can interrupt idle socket I/O without needing an event; it must remain set until return. Cancelled requests never return partial `Response`s. `Cancelled` is a public standard error type (`error.is::<Cancelled>()`).
- `encode(Provider, &Request) -> Result<Value>`: build the native JSON body without networking.
- `decode(Provider, model, impl BufRead, emit) -> Result<Response>`: consume an SSE reader without making a request.
- `Request`, `Message`, `Response`, `Result<T>` and `LIMIT` (16 MiB) are public. Errors are boxed, thread-safe standard errors.

See [FORMATS.md](FORMATS.md) for fields, events, native examples, and replay.

| Provider | Default target | Authentication |
| --- | --- | --- |
| OpenAI | `https://api.openai.com/v1/responses` | Bearer token |
| OpenRouter | `https://openrouter.ai/api/v1/responses` | Bearer token |
| Anthropic | `https://api.anthropic.com/v1/messages` | `x-api-key`; `anthropic-version: 2023-06-01` |
| Codex subscription | `https://chatgpt.com/backend-api/codex/responses` | Bearer access token and `chatgpt-account-id`; `originator: llm-api`, `OpenAI-Beta: responses=experimental`, `Accept: text/event-stream` |

OpenRouter uses stateless Responses, not server-side conversation storage. The shared Responses encoder forces `store: false` and requests `include: ["reasoning.encrypted_content"]`; callers supply full history with native output replay. The [official OpenRouter Responses overview](https://openrouter.ai/docs/api_reference/responses/overview.md) documents encrypted reasoning and tool calling, and rejects `store: true` or non-null `previous_response_id`. Use `store: false` and omit `previous_response_id`; the client does not remove a caller-supplied ID from native options.

Anthropic replay retains only `role: "assistant"` and assembled `content`, stripping response-only envelope fields while preserving thinking signatures and redacted blocks. Responses replay is an output-item array bound to the original provider and model; `codex` and `openai` are distinct providers even for the same model string.

Codex uses the shared Responses encoder with subscription-specific adjustments: nonempty `instructions` (fallback `"You are a helpful assistant."` for blank system text), forced `parallel_tool_calls: true`, and defaults `tool_choice: "auto"` and `text.verbosity: "low"` that preserve explicit native choices. `store: false` and `stream: true` are forced. Common `max_tokens` remains required and positive, but Codex removes `max_output_tokens`, including a value supplied in native options. It does not enforce a token cap; model/server limits apply. Use short prompts, low reasoning effort where supported, callback cancellation, and an application time limit rather than relying on `max_tokens` to bound Codex generation.

## Data flow and boundaries

1. `encode` starts with the provider-native `options` object. Standard fields overwrite their corresponding native fields; nested settings such as `text` and `output_config` are merged where implemented. This is not a general options validator or portable reasoning abstraction.
2. Messages become native text, inline images, tool calls/results, or native replay preserving protocol-specific content. Tool declarations and output schemas are wrapped for the selected protocol; their schema subsets are not transformed to fit provider capabilities.
3. `run` serializes and bounds the request, sends one POST, checks HTTP success, and reads SSE. There is no application retry loop. Redirects are disabled; the global request timeout is 180 seconds.
4. `decode` reads `data:` records, joining multiline data and ignoring other SSE fields. Framing accepts LF and CRLF, including an initial UTF-8 BOM; standalone-CR framing is not supported. It handles provider errors and terminal markers, emits normalized deltas, and assembles native output. OpenAI, Codex, and OpenRouter share the Responses encoder/decoder. It accepts `response.done`, `response.completed`, and `response.incomplete` with consistent completed/incomplete statuses; failed, cancelled, in-progress terminal statuses and provider errors fail. Missing/null terminal output is recovered only from finalized `response.output_item.done` items, never raw partial deltas; pending items or no finalized items fail recovery.
5. Finalization parses tool argument JSON, builds normalized content and usage, and attaches provider/model-bound native replay. The caller appends the returned assistant message and any tool results to the next request.

An event callback runs only when a normalized event is emitted. `run` therefore has callback-only cancellation, whereas `run_cancellable` also observes a caller-supplied atomic flag while socket I/O is blocked. Its monitor shuts down only the per-call `ureq` agent, wakes on normal completion/error/unwinding, and is joined before return; it does not move callbacks onto a worker or require `Send`. Cancellation is checked before connecting, before emitting, and after the request attempt so shutdown-induced EOF cannot become success. Completed calls and a racing cancellation are not transactionally ordered; callers must not reset the flag until return.

Socket shutdown covers established TCP, TLS negotiation, response headers, and body reads. DNS and TCP connect are instead bounded by five-second stage timeouts, subject to the 180-second global budget. The default resolver may leave an OS lookup thread running after its waiter times out; the client does not promise hard preemption of DNS, CPU work, or application callbacks. No automatic retries or remote cancellation request is sent, and remote generation/billing may continue. Cancelled calls discard final output even if some deltas were already emitted. A separate transport per cancellable call trades pooling for cancellation isolation; ordinary `run` retains its pool. `ureq` 3.5.1 is the minimum supported version for socket shutdown.

The JSONL process remains sequential with no in-band cancellation command. This portable cancellation API is for Rust callers; other-language callers can still own and terminate their subprocess.

Requests serialized by `run` and SSE bytes read by `decode` each have a 16 MiB limit. This is a wire/input bound, not a guarantee that all in-memory representations together fit in 16 MiB. Missing terminal events are errors. Incomplete or malformed tool argument JSON is also an error, even after deltas have been delivered and even on a length-limited generation. A successful `length` response may still contain partial text or syntactically valid but semantically incomplete arguments; callers must decide whether to use them.

## Security and ownership

The caller owns endpoint trust, credentials, model selection, native options, history, and tool authorization. HTTPS alone does not make an endpoint trustworthy: an override receives the key and conversation. A service exposing its own credentials must not let untrusted callers control endpoints or arbitrary native options; constrain these at the service boundary, along with models and other resource controls.

- `Client` has no `Debug` or serialization implementation for its stored credentials. The crate never reads fx authentication files, the home directory, or `.env`, and never refreshes tokens. Credentials are supplied by the caller.
- No redirects, tool execution, image URL fetching, or automatic retries are implemented.
- Images contain caller-supplied base64 strings and MIME types. Only the `image/` MIME prefix and string fields are checked; image bytes/base64 are not decoded or inspected.
- Tool schemas and output schemas are forwarded, not locally enforced. Generated JSON remains text, not a locally parsed/schema-validated result. Tool argument JSON is parsed and required to be an object, but is not checked against the tool's parameter schema.
- Treat text, reasoning, tool arguments/results, and replay as sensitive, untrusted data. Preserve replay without rewriting or executing it. Replay is not cross-provider or cross-model portable.
- HTTP error bodies and provider stream-error payloads are omitted from errors to reduce secret exposure. This is not a blanket sanitization guarantee for all errors or application logs.

These documents describe the current local implementation. [LIVE_TESTS.md](LIVE_TESTS.md) records successful model-specific live checks for all four modes, including the existing fx subscription login. Examples remain illustrative rather than individually verified provider requests.

## Subprocess portability

`src/main.rs` exposes the same blocking client to languages that can spawn a process and exchange UTF-8 JSON lines. Build with `cargo build --release`; the executable is `target/release/llm-api`. These are usage commands, not verification results.

Send one command object per stdin line:

```json
{"provider":"openai","request":{"model":"MODEL_ID","messages":[{"role":"user","content":[{"type":"text","text":"Hello"}]}],"max_tokens":256}}
```

An optional command-level `endpoint` overrides the target. CLI credentials are read from the environment, never from the command:

| Provider | Variable |
| --- | --- |
| OpenAI | `OPENAI_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Anthropic | `ANTHROPIC_API_KEY` |
| Codex subscription | Both `CODEX_ACCESS_TOKEN` and `CODEX_ACCOUNT_ID` |

`tests/codex_session.py` is an explicit, read-only live-test bridge, not crate or CLI credential discovery. It reads `FX_HOME/chatgpt-auth.json` when `FX_HOME` is set, otherwise `~/.fx/chatgpt-auth.json`, checks a private user-owned regular file and version-1 session, and supplies the two Codex environment variables. An expired or near-expired token (within 60 seconds) stops the bridge: no refresh and no file write. Renew the login separately before retrying.

Stdout contains flushed JSONL events. For each nonblank, ordinary-sized input line, processing is sequential: zero or more deltas followed by either `{"type":"done","response":...}` or `{"type":"error","message":"..."}`. There are no request IDs and no interleaving. The library itself returns `Response`; only the CLI wraps it in `done`.

Malformed JSON, unknown command/request/message fields, unsupported provider names, missing keys, encoding failures, and provider failures produce an `error` line, then the process handles the next command. An error may follow already-emitted deltas; it has no `done`. Blank lines are ignored. A command line over 16 MiB, input read/UTF-8 failure, or unrecoverable stdout failure terminates the process instead of guaranteeing a per-line error event. Applications must handle EOF/process failure as well as JSONL errors. Do not assume exit success means every command succeeded.
