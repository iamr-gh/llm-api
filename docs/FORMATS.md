# Formats

Implementation reference for `src/lib.rs` and the JSONL wrapper in `src/main.rs`. See [live results](LIVE_TESTS.md) for the verified OpenRouter free-model scope and OpenAI quota blocker. Examples below remain illustrative wire shapes, not individually verified provider requests. `MODEL_ID`, encrypted data, and signatures stand for values from the selected model/provider; do not manufacture replay in production.

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

Use assistant messages for tool calls and user messages for results. `is_error` is forwarded only to Anthropic (default `false`). Result strings are preserved; other JSON values are serialized to strings for Responses and Anthropic. Gemini keeps the value under `functionResponse.response.result`, and finds the function name by matching the result ID to a preceding normalized tool call in request history. Calls are registered before processing each message's replay, so replay-bearing assistant messages also supply names; a result without a preceding call is an encoding error. Retain the returned assistant `content` even when using replay, so this lookup can work.

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

On encoding, replay requires the original provider, exact model string, and assistant role. Its native data replaces that message's normalized content. Responses data is an array of output items spliced into `input`; Anthropic replays a native message containing only `role: "assistant"` and `content`, while Gemini replays a model content object with `role` and `parts`. This preserves encrypted reasoning, thinking signatures, thought signatures, redacted blocks, and other native content that normalized text cannot represent. Replay is opaque and explicitly not cross-provider/model portable; visible reasoning alone cannot reconstruct it. Matching the envelope does not independently validate or authenticate the native payload.

### Native reasoning options

`options` contains provider-native top-level fields. The encoder starts from these options and then sets standard request fields; it is not an unrestricted override of those fields. Nested objects `text`, `output_config`, and `generationConfig` must be objects when the encoder accesses them.

| Protocol | Example `options` |
| --- | --- |
| OpenAI Responses | `{"reasoning":{"effort":"medium","summary":"auto"}}` |
| Anthropic Messages | `{"thinking":{"type":"enabled","budget_tokens":1024}}` with `max_tokens: 4096` |
| Gemini | `{"generationConfig":{"thinkingConfig":{"thinkingBudget":1024,"includeThoughts":true}}}` |
| OpenRouter Responses | `{"reasoning":{"effort":"medium","summary":"auto"}}` |

Anthropic's enabled thinking budget must be below `max_tokens` and satisfy the model's requirements; the client does not check this relation. OpenAI, xAI, and OpenRouter share the Responses encoder, but supported reasoning controls remain provider/model-specific. OpenRouter uses Responses reasoning configuration, not route-specific chat reasoning fields. These options are examples, not capability negotiation: provider/model support, budget limits, and valid combinations must be checked by the caller.

## Native wire formats

The examples below show the *next* request after a tool call, including replayed reasoning and a tool result. They also show the output-schema wrapper. `parameters`/`input_schema` and the output schema are passed through unchanged: the client does not rewrite unsupported schema subsets. Whether a provider/model accepts schemas together with tools and reasoning is not locally verified.

There are three wire protocols across the five providers: Responses, Anthropic Messages, and Gemini streamGenerateContent.

### OpenAI, xAI, and OpenRouter: Responses

POST to `https://api.openai.com/v1/responses`, `https://api.x.ai/v1/responses`, or `https://openrouter.ai/api/v1/responses` with Bearer authentication. The encoder always sets `stream: true`, `store: false`, and `include: ["reasoning.encrypted_content"]`. `max_tokens` becomes `max_output_tokens`; `system` becomes `instructions`.

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
  "thinking":{"type":"enabled","budget_tokens":1024},
  "output_config":{"format":{"type":"json_schema","schema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false}}},
  "max_tokens":4096,
  "stream":true
}
```

Replay `data` contains only `{"role":"assistant","content":[...]}` from the assembled native message. Response-only envelope fields such as `id`, `type`, `model`, `stop_reason`, `stop_sequence`, and `usage` are stripped; all content blocks, including thinking signatures and redacted thinking, are preserved. Images use `{"type":"image","source":{"type":"base64","media_type":"image/png","data":"..."}}`. Tool input is an object on this wire; streamed `partial_json` arguments are assembled and parsed before final output.

### Gemini: streamGenerateContent

POST to `/v1beta/models/MODEL_ID:streamGenerateContent?alt=sse` with `x-goog-api-key`. There is no body-level `model` or `stream` field added by the encoder. Assistant messages become `role: "model"`.

```json
{
  "contents":[
    {"role":"user","parts":[{"text":"Look up Paris."}]},
    {"role":"model","parts":[{"text":"I need a lookup.","thought":true},{"functionCall":{"id":"c1","name":"lookup","args":{"city":"Paris"}},"thoughtSignature":"PROVIDER_RETURNED_SIGNATURE"}]},
    {"role":"user","parts":[{"functionResponse":{"id":"c1","name":"lookup","response":{"result":{"temperature":20}}}}]}
  ],
  "systemInstruction":{"parts":[{"text":"Be concise."}]},
  "tools":[{"functionDeclarations":[{"name":"lookup","description":"Look up a city","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"],"additionalProperties":false}}]}],
  "generationConfig":{
    "maxOutputTokens":4096,
    "thinkingConfig":{"thinkingBudget":1024,"includeThoughts":true},
    "responseMimeType":"application/json",
    "responseJsonSchema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false}
  }
}
```

Replay `data` is the assembled model content (`role` and `parts`), including original thought signatures. Normalized function IDs are synthesized as `call_{index}` if missing; do not add those synthetic IDs to native replay yourself. Images become `{"inlineData":{"mimeType":"image/png","data":"..."}}`. The function response name is recovered by ID from preceding normalized tool-call history, including replay-bearing assistant messages, not from replay inspection. Keep their normalized `content`; later calls cannot supply a missing name for an earlier result.

## Streaming events and final response

Provider input is SSE; the callback and subprocess output use JSON objects, not raw SSE. Recognized normalized incremental events are:

```json
{"type":"text_delta","index":0,"delta":"Hello"}
{"type":"reasoning_delta","index":0,"delta":"Visible thought or summary"}
{"type":"tool_call_start","index":1,"id":"c1","name":"lookup"}
{"type":"tool_arguments_delta","index":1,"delta":"{\"city\":"}
```

`index` is provider-local: Responses output-item index (OpenAI, xAI, OpenRouter), Anthropic block index, or accumulated Gemini part index. It is not a portable content-array index. Argument deltas can be incomplete JSON. Do not execute from deltas. Not every native field becomes an event: signatures and encrypted reasoning are retained in replay; some content arriving in start/final payloads may have no corresponding delta.

Terminal detection uses Responses `response.completed`/`response.incomplete` for OpenAI, xAI, and OpenRouter, Anthropic `message_stop` plus a stop reason, or Gemini `finishReason` (the decoder continues reading Gemini to EOF). Missing completion information is an error, not silent success. Provider error records also fail the request.

A library success returns this shape; the CLI emits it as `{"type":"done","response":...}`:

```json
{
  "message":{"role":"assistant","content":[{"type":"text","text":"{\"answer\":\"20 degrees\"}"}],"replay":{"provider":"gemini","model":"MODEL_ID","data":{"role":"model","parts":[{"text":"{\"answer\":\"20 degrees\"}"}]}}},
  "finish_reason":"stop",
  "usage":{"normalized":{"input_tokens":12,"output_tokens":8,"cached_tokens":null,"reasoning_tokens":null},"native":{"promptTokenCount":12,"candidatesTokenCount":8}}
}
```

JSON-schema output is still a text block. Neither JSON parsing nor schema validation is performed on generated answer text.

Finish reasons are partially normalized, not a closed enum:

- Responses completed → `stop`; incomplete uses `incomplete_details.reason` (or `incomplete` if absent).
- `end_turn` and `STOP` → `stop`; `max_tokens`, `max_output_tokens`, and `MAX_TOKENS` → `length`; `tool_use` → `tool_calls`.
- After these mappings, a parsed tool call changes `stop` to `tool_calls`. Length limits and other reasons are preserved even when tool calls are present.
- Other native reasons pass through. Text can be partial on `length`. Invalid/incomplete tool argument JSON fails finalization even on truncation; syntactically valid partial arguments are not schema-validated.

## Usage semantics

`usage.normalized` always has four keys. Missing/unknown counts are JSON `null`, not zero. `usage.native` retains the provider usage object for interpretation.

| Normalized field | Responses (OpenAI, xAI, OpenRouter) | Anthropic | Gemini |
| --- | --- | --- | --- |
| `input_tokens` | `input_tokens` | `input_tokens` | `promptTokenCount` |
| `output_tokens` | `output_tokens` | `output_tokens` | `candidatesTokenCount` |
| `cached_tokens` | `input_tokens_details.cached_tokens` | `cache_read_input_tokens` | `cachedContentTokenCount` |
| `reasoning_tokens` | `output_tokens_details.reasoning_tokens` | Usually absent (`null`) | `thoughtsTokenCount` |

These are field mappings, not equivalent billing/accounting definitions. Cache reads, cache creation, whether cached tokens are included in input totals, and whether thinking is included in output totals differ by provider. Anthropic cache-creation fields remain native; Gemini thought counts are separate native metrics. Do not add or compare normalized counts as if they had identical semantics, and consult native usage for provider-specific details.
