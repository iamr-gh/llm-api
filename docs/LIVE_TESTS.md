# Live verification

Observed on November 4, 2026. Tests used synthetic prompts and a generated 32×32 red PNG. No keys, authorization headers, or raw replay payloads are recorded here. Credentials were read from the local, Git-ignored `.env` file.

## Results

| Provider/model | Result | Boundary |
| --- | --- | --- |
| OpenRouter `nvidia/nemotron-3.5-super-vl:free` | Passed all five feature checks across seven generation requests | Catalog prompt/completion pricing was zero; every request in the final suite reported native cost `0` |
| OpenAI `gpt-5.4-nano` | Generation blocked by account balance | Authenticated model listing succeeded; generation returned `insufficient_quota` / `credit_balance_exhausted` |
| Anthropic, Gemini, xAI | Not live tested | Offline fixtures only; no credentials supplied for these providers |

The successful OpenRouter run used 887 reported input tokens and 487 output tokens in total, with a summed reported cost of `0.0`. Earlier OpenRouter diagnostics also used only the same verified free model. OpenAI was not retried after confirming the quota failure. Its generated-output compatibility remains unverified.

### OpenRouter assertions

| Check | Actual assertion | Final finish reason |
| --- | --- | --- |
| Text streaming | Exact `LIVE_OK` answer and incremental text event | `stop` |
| Structured JSON | Parsed answer equals `{"ok": true}` with the supplied strict schema | `stop` |
| Tool call | One `add` call with object arguments `{"a": 2, "b": 3}` and streamed argument deltas | `tool_calls` |
| Tool replay | Append untouched native replay plus matching tool result, receive exact `5` | `stop` |
| Image | Identify the generated red PNG as red | `stop` |
| Reasoning | Exact `323` answer and a native reasoning item | `stop` |
| Reasoning replay | Append the returned message and follow-up, receive exact `324` | `stop` |

These are smoke checks, not a comprehensive provider conformance test or proof that every schema constraint is enforced. Model capabilities and free-model availability can change.

## Bug found and fixed

OpenRouter's Responses output can carry visible reasoning under a `reasoning` item's `content` array, rather than only under `summary`. The initial client classified that content as answer text, contaminating otherwise-valid JSON answers. Streaming answer text itself was correct.

The shared Responses adapter now classifies reasoning content separately and recognizes `response.reasoning_text.delta` as a normalized `reasoning_delta`. Native replay remains intact. `tests/responses_reasoning.rs` reproduces the original failure and passes after the fix. All 22 offline tests pass. Production Rust remains below the requested bound at 694 physical lines after formatting.

## Reproduce

From the repository directory, put `OPENROUTER_API_KEY` and/or `OPENAI_API_KEY` in `.env`. The runner reads only these two assignments; it does not execute the file. Do not put keys in commands or checked-in files.

```sh
cargo build --release --locked --offline
python3 tests/live.py --list
python3 -u tests/live.py --provider openrouter --model nvidia/nemotron-3.5-super-vl:free
```

The final OpenRouter command exited `0`. The runner verifies a `:free` suffix and zero catalog prompt/completion prices before permitting any OpenRouter generation. It never selects a paid fallback. If this model is unavailable, inspect `--list` and explicitly select another free model with the required capabilities.

After funding the OpenAI account, retry the low-cost model explicitly. This command can incur charges and was blocked during this verification:

```sh
python3 -u tests/live.py --provider openai --model gpt-5.4-nano
```

The OpenAI smoke command exited `1`. An additional `--diagnose` request confirmed `credit_balance_exhausted`; the diagnostic command itself exited `0` because it successfully inspected the stream, not because generation succeeded.

Use `--features text`, `--features json,tools`, or another subset to limit requests. `--diagnose` sends a single short synthetic text request directly to the Responses endpoint to report sanitized provider errors and event types. Diagnostics are separate from the library's intentionally payload-free errors. Neither mode runs during `cargo test`.

Offline verification after the fix, all exit `0`:

```sh
cargo test --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
cargo build --release --locked --offline
```

The test runner uses only Python's standard library. `python3 -m py_compile tests/live.py` also passed. Actual live results are scoped to this model and credential/account state, not the entire five-provider API surface.
