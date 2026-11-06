# Formats

Implementation reference for `src/lib.rs` and the JSONL wrapper in `src/main.rs`. See [LIVE_TESTS.md](LIVE_TESTS.md) for model-specific evidence covering all four modes. Examples below remain illustrative wire shapes, not individually verified provider requests. `MODEL_ID`, encrypted data, and signatures stand for values from the selected model/provider; do not manufacture replay in production.

## Standard request and messages

```json
{
  "model": "MODEL_ID",
  "system": "Be concise.",
  "messages": [
    {"role":"user","content":[
      {"type":"text","text":"What is here?"},
      {"type":"image","mime_type":"image/png","data":"iVBORw0KGgo="}
    ]}
  ],
  "tools": [{"name":"lookup","description":"Look up a city","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"],"additionalProperties":false}}],
  "json_schema": {"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false},
  "max_tokens": 4096,
  "options": {}
}
```

The image string above is only an abbreviated payload, not a complete image. Supply complete base64 image bytes with their MIME type; the client never fetches a URL.

`model`, `messages`, and positive `max_tokens` are required. `system` defaults to `""`, `tools` to `[]`, `json_schema` to `null`, and `options` to `{}`. Unknown fields on `Request` and `Message` are rejected; content blocks and tool declarations are JSON values with only selected fields checked.

A `Message` has `role` (`user` or `assistant`), a required `content` array, and optional `replay`. Put system instructions in `Request.system`, not a system-role message.

| Block | Shape / behavior |
| --- | --- |
| Text | `{"type":"text","text":"Hello"}` |
| Image | `{"type":"image","mime_type":"image/jpeg","data":"BASE64_BYTES"}`; MIME must start with `image/` |
| Tool call | `{"type":"tool_call","id":"c1","name":"lookup","arguments":{"city":"Paris"}}` |
| Tool result | `{"type":"tool_result","id":"c1","output":{"temperature":20},"is_error":false}` |
| Reasoning output | `{"type":"reasoning","text":"Visible reasoning or summary"}` |
| Refusal output | `{"type":"refusal","text":"..."}` (Responses output only) |

Use assistant messages for tool calls and user messages for results. `is_error` is forwarded only to Anthropic (default `false`). Result strings are preserved; other JSON values are serialized to strings for Responses and Anthropic.

Reasoning is output, not portable input: a raw `reasoning` history block without replay is rejected. Refusal blocks also have no normalized input encoder; use the returned replay.

### Replay and tool loop

Every successful response includes an assistant message with this envelope:

```json
{
  "role":"assistant",
  "content":[{"type":"reasoning","text":"Visible summary"},{"type":"tool_call","id":"c1","name":"lookup","arguments":{"city":"Paris"}}],
  "replay":{"provider":"openai","model":"MODEL_ID","data":[]}
}
```

Here `data: []` only illustrates the envelope; real `data` comes from the response and must be retained intact. Append the entire returned `message`, then a user message containing the matching `tool_result`, and run again. Tool execution is the caller's responsibility.

On encoding, replay requires the original provider, exact model string, and assistant role. Its native data replaces that message's normalized content. Responses data is an array of output items spliced into `input`; Anthropic replays a native message containing only `role: "assistant"` and `content`. Codex uses Responses replay with provider `codex`, not `openai`. This preserves encrypted reasoning, thinking signatures, redacted blocks, and other native content that normalized text cannot represent. Replay is opaque and explicitly not cross-provider/model portable; visible reasoning alone cannot reconstruct it. Matching the envelope does not independently validate or authenticate the native payload.

### Native reasoning options

`options` contains provider-native top-level fields. The encoder starts from these options and then sets standard request fields; it is not an unrestricted override of those fields. Nested objects `text` and `output_config` must be objects when the encoder accesses them.

| Protocol | Example `options` |
| --- | --- |
| OpenAI Responses | `{"reasoning":{"effort":"medium","summary":"auto"}}` |
| Anthropic adaptive thinking | `{"thinking":{"type":"adaptive"},"output_config":{"effort":"medium"}}` on supporting models |
| Anthropic older enabled thinking | `{"thinking":{"type":"enabled","budget_tokens":1024}}` with `max_tokens: 4096` on supporting models |
| Codex subscription Responses | `{"reasoning":{"effort":"low"}}` |
| OpenRouter Responses | `{"reasoning":{"effort":"medium","summary":"auto"}}` |

Anthropic models differ: the live-tested Haiku 5.5 requires adaptive thinking and rejects the older enabled-budget mode. When a model supports enabled thinking, its budget must be below `max_tokens` and satisfy its requirements; the client does not check this relation. The test runner consults the model capability metadata; the crate deliberately does not maintain a capability catalog. OpenAI, Codex, and OpenRouter share the Responses encoder, but supported reasoning controls remain provider/model-specific. OpenRouter uses Responses reasoning configuration, not route-specific chat reasoning fields. These options are examples, not capability negotiation: provider/model support, budget limits, and valid combinations must be checked by the caller.

## Native wire formats

The examples below show the *next* request after a tool call, including replayed reasoning and a tool result. They also show the output-schema wrapper. `parameters`/`input_schema` and the output schema are passed through unchanged: the client does not rewrite unsupported schema subsets. Whether a provider/model accepts schemas together with tools and reasoning is not locally verified.

There are two wire protocols: Responses (OpenAI, OpenRouter, and Codex subscription mode) and Anthropic Messages.

### OpenAI and OpenRouter: API-key Responses

POST to `https://api.openai.com/v1/responses` or `https://openrouter.ai/api/v1/responses` with Bearer authentication. The encoder always sets `stream: true`, `store: false`, and `include: ["reasoning.encrypted_content"]`. `max_tokens` becomes `max_output_tokens`; `system` becomes `instructions`.

```json
{
  "model":"MODEL_ID",
  "instructions":"Be concise.",
  "input":[
    {"role":"user","content":[{"type":"input_text","text":"Look up Paris."}]},
    {"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"I need a lookup."}],"encrypted_content":"PROVIDER_RETURNED_CIPHERTEXT"},
    {"type":"function_call","id":"fc_1","call_id":"c1","name":"lookup","arguments":"{\"city\":\"Paris\"}"},
    {"type":"function_call_output","call_id":"c1","output":"{\"temperature\":20}"}
  ],
  "tools":[{"type":"function","name":"lookup","description":"Look up a city","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"],"additionalProperties":false},"strict":false}],
  "reasoning":{"effort":"medium","summary":"auto"},
  "text":{"format":{"type":"json_schema","name":"result","strict":true,"schema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false}}},
  "max_output_tokens":4096,
  "stream":true,
  "store":false,
  "include":["reasoning.encrypted_content"]
}
```

The reasoning and function-call items are the previous response's `output` array, retained as replay `data`. Tool arguments are JSON strings on this wire. Normalized assistant text becomes `output_text`; user text becomes `input_text`. Images become `{"type":"input_image","image_url":"data:image/png;base64,..."}`. All three providers use this same mapping and replay output items, including encrypted reasoning and function calls; reasoning options and schema/tool support remain provider/model-specific.

OpenRouter Responses is stateless: send the full history on every request with `store: false`. The [official overview](https://openrouter.ai/docs/api_reference/responses/overview.md) documents encrypted reasoning and tool calling and rejects `store: true` or non-null `previous_response_id`. The encoder forces `store: false` but does not strip `previous_response_id` from native options; omit that field.

### Codex: subscription Responses

Public `Provider::Codex` serializes as `"codex"`. Construct with `Client::codex(access_token, account_id) -> Result<Self>`, not an API key. POST to `https://chatgpt.com/backend-api/codex/responses` with `Authorization: Bearer ACCESS_TOKEN`, `chatgpt-account-id: ACCOUNT_ID`, `originator: llm-api`, `OpenAI-Beta: responses=experimental`, and `Accept: text/event-stream`.

Codex shares the Responses input, tools, schema, encrypted-reasoning include, and replay mappings above, with these adjustments:

- `max_tokens` is still a required positive common request field, but `max_output_tokens` is removed even if supplied in `options`. The client does not enforce a Codex token cap; model/server limits apply.
- Blank/whitespace-only `system` gets nonempty fallback instructions `"You are a helpful assistant."`; otherwise `system` becomes `instructions` unchanged.
- `parallel_tool_calls: true`, `store: false`, and `stream: true` are forced.
- `tool_choice` defaults to `"auto"` and `text.verbosity` to `"low"`; explicit native option choices are preserved. `text` must be an object.

Illustrative encoded body for a short prompt with no tools:

```json
{
  "model":"MODEL_ID",
  "instructions":"You are a helpful assistant.",
  "input":[{"role":"user","content":[{"type":"input_text","text":"Reply OK."}]}],
  "tools":[],
  "reasoning":{"effort":"low"},
  "text":{"verbosity":"low"},
  "tool_choice":"auto",
  "parallel_tool_calls":true,
  "store":false,
  "stream":true,
  "include":["reasoning.encrypted_content"]
}
```

Use short prompts and low reasoning effort where supported, with callback cancellation and an application time limit for resource control. Cancellation is checked only on normalized events, cannot interrupt an idle read, and returns an error rather than a partial response; the HTTP global timeout is 180 seconds. Do not rely on common `max_tokens` to limit Codex generation. Replay is bound to `codex` and the exact model; an `openai` replay is rejected even for the same model.

The credential-holding `Client` implements neither `Debug` nor serialization. The crate never reads fx files, the home directory, or `.env`, and never refreshes credentials. The CLI requires both `CODEX_ACCESS_TOKEN` and `CODEX_ACCOUNT_ID`. Only the explicitly invoked live-test bridge `tests/codex_session.py` reads `FX_HOME/chatgpt-auth.json` or, by default, `~/.fx/chatgpt-auth.json`. It is read-only and stops on an expired or near-expired token (within 60 seconds), without refresh or file writes; renew the login separately.

### Anthropic: Messages

POST to `/v1/messages` with `x-api-key` and the pinned `anthropic-version: 2023-06-01`. `system` and `max_tokens` remain top-level fields.

```json
{
  "model":"MODEL_ID",
  "system":"Be concise.",
  "messages":[
    {"role":"user","content":[{"type":"text","text":"Look up Paris."}]},
    {"role":"assistant","content":[{"type":"thinking","thinking":"I need a lookup.","signature":"PROVIDER_RETURNED_SIGNATURE"},{"type":"tool_use","id":"c1","name":"lookup","input":{"city":"Paris"}}]},
    {"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"{\"temperature\":20}","is_error":false}]}
  ],
  "tools":[{"name":"lookup","description":"Look up a city","input_schema":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"],"additionalProperties":false}}],
  "thinking":{"type":"adaptive"},
  "output_config":{"effort":"medium","format":{"type":"json_schema","schema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false}}},
  "max_tokens":4096,
  "stream":true
}
```

Replay `data` contains only `{"role":"assistant","content":[...]}` from the assembled native message. Response-only envelope fields such as `id`, `type`, `model`, `stop_reason`, `stop_sequence`, and `usage` are stripped; all content blocks, including thinking signatures and redacted thinking, are preserved. Images use `{"type":"image","source":{"type":"base64","media_type":"image/png","data":"..."}}`. Tool input is an object on this wire; streamed `partial_json` arguments are assembled and parsed before final output.

## Streaming events and final response

Provider input is SSE; the callback and subprocess output use JSON objects, not raw SSE. Recognized normalized incremental events are:

```jsonl
{"type":"text_delta","index":0,"delta":"Hello"}
{"type":"reasoning_delta","index":0,"delta":"Visible thought or summary"}
{"type":"tool_call_start","index":1,"id":"c1","name":"lookup"}
{"type":"tool_arguments_delta","index":1,"delta":"{\"city\":"}
```

`index` is provider-local: Responses output-item index (OpenAI, Codex, OpenRouter) or Anthropic block index. It is not a portable content-array index. Argument deltas can be incomplete JSON. Do not execute from deltas. Not every native field becomes an event: signatures and encrypted reasoning are retained in replay; some content arriving in start/final payloads may have no corresponding delta.

Terminal detection accepts Responses `response.completed`, `response.incomplete`, or `response.done`. A completed/incomplete event must agree with its response status; `response.done` uses the supplied completed/incomplete status; if absent, it infers incomplete from non-null `incomplete_details`, otherwise completed. Failed, cancelled, in-progress, conflicting terminal statuses, non-null response errors, and provider error records fail the request. Anthropic requires `message_stop` plus a stop reason. Missing completion information is an error, not silent success.

If a Responses terminal payload omits `output` or sets it to null, recovery requires at least one finalized `response.output_item.done` item and no pending items from `response.output_item.added`; items are ordered by output index. Text/reasoning/argument deltas alone never become replay or a successful final response. An explicit terminal output array is used as supplied.

A library success returns this shape; the CLI emits it as `{"type":"done","response":...}`:

```json
{
  "message":{"role":"assistant","content":[{"type":"text","text":"{\"answer\":\"20 degrees\"}"}],"replay":{"provider":"openai","model":"MODEL_ID","data":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"{\"answer\":\"20 degrees\"}"}]}]}},
  "finish_reason":"stop",
  "usage":{"normalized":{"input_tokens":12,"output_tokens":8,"cached_tokens":null,"reasoning_tokens":null},"native":{"input_tokens":12,"output_tokens":8}}
}
```

JSON-schema output is still a text block. Neither JSON parsing nor schema validation is performed on generated answer text.

Finish reasons are partially normalized, not a closed enum:

- Responses completed → `stop`; incomplete uses `incomplete_details.reason` (or `incomplete` if absent).
- `end_turn` → `stop`; `max_tokens`, `max_output_tokens` → `length`; `tool_use` → `tool_calls`.
- After these mappings, a parsed tool call changes `stop` to `tool_calls`. Length limits and other reasons are preserved even when tool calls are present.
- Other native reasons pass through. Text can be partial on `length`. Invalid/incomplete tool argument JSON fails finalization even on truncation; syntactically valid partial arguments are not schema-validated.

## Usage semantics

`usage.normalized` always has four keys. Missing/unknown counts are JSON `null`, not zero. `usage.native` retains the provider usage object for interpretation.

| Normalized field | Responses (OpenAI, Codex, OpenRouter) | Anthropic |
| --- | --- | --- |
| `input_tokens` | `input_tokens` | `input_tokens` |
| `output_tokens` | `output_tokens` | `output_tokens` |
| `cached_tokens` | `input_tokens_details.cached_tokens` | `cache_read_input_tokens` |
| `reasoning_tokens` | `output_tokens_details.reasoning_tokens` | Usually absent (`null`) |

These are field mappings, not equivalent billing/accounting definitions. Cache reads, cache creation, whether cached tokens are included in input totals, and whether thinking is included in output totals differ by provider. Anthropic cache-creation fields remain native. Do not add or compare normalized counts as if they had identical semantics; consult native usage for provider-specific details.
