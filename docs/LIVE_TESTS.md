# Live verification

Latest verification, November 5, 2026. Scope is OpenAI, Anthropic, and OpenRouter API keys, plus a separate Codex subscription mode. Tests use synthetic prompts and generated images. No secrets, authorization headers, account IDs, or raw replay payloads are recorded here.

## Current results

Every mode passed text streaming, JSON-schema answer output, function calling and tool-result replay, image input, and native reasoning with conversation replay. These are model-specific smoke checks, not full API conformance or comprehensive schema validation.

| Mode | Model | Result | Credentials |
| --- | --- | --- | --- |
| OpenAI API | `gpt-5.4-nano` | All feature checks passed across targeted runs | `.env` API key |
| Anthropic API | `claude-haiku-5-5` | All feature checks passed across targeted runs | `.env` API key |
| OpenRouter | `nvidia/nemotron-3.5-super-vl:free` | All feature checks passed; every generation reported cost `0` | `.env` API key |
| Codex subscription | `gpt-6.1-sol` | All feature checks passed using the active fx login | Read-only fx access-token/account-ID bridge |

The OpenAI credit-exhaustion blocker recorded on November 4 is resolved with the current key. Model choices were checked against authenticated catalogs. Anthropic's current Haiku capability metadata requires adaptive thinking rather than `thinking.type: enabled`; the live runner consults this metadata. Codex model discovery uses the same backend catalog and client-version query pattern found in the fx source.

### Usage this round

Counts include successful generation responses whose test assertions initially failed, not just final passing assertions. Catalog requests are not generation requests. Counts differ in semantics by provider, and the normalized fields are not a billing calculation.

| Mode | Generation requests | Reported input tokens | Reported output tokens | Reported dollar cost |
| --- | ---: | ---: | ---: | --- |
| OpenAI API | 8 | 484 | 742 | Not supplied by the response |
| Anthropic API | 9 | 2,292 | 1,446 | Not supplied by the response |
| OpenRouter free | 9 | 1,064 | 1,805 | `0.0` |
| Codex subscription | 8 | 616 | 199 | Not supplied; subscription quota applies |

No paid OpenRouter model or fallback was used. Exact OpenAI/Anthropic charges require the providers' billing records; they are not inferred from missing cost fields.

## Assertions and adjustments

- Text must include an incremental `text_delta` and the exact synthetic answer `LIVE_OK`.
- JSON-schema output must parse to `{"ok": true}`.
- Tool tests require one streamed `add` call with arguments `{"a": 2, "b": 3}`. Replaying the untouched assistant message plus its matching tool result must produce `5`.
- Image tests identify a generated solid-red PNG. OpenAI Nano did not satisfy the color assertion on the initial 32×32 fixture; the 256×256 fixture passed. The other modes passed the original small fixture. No image-encoding change was needed.
- Adaptive reasoning is not guaranteed to emit thoughts for trivial arithmetic. Haiku and Codex answered the initial `17 * 19` question without a reasoning item, which is legitimate behavior rather than an adapter failure.
- The final reasoning check counts numbers in `1..=1,000,000` satisfying three modular constraints and excluding multiples of five. The expected count, independently computed in Python, is `799`; the replay follow-up must return `800`.
- All four modes returned native reasoning for the nontrivial question and accepted the returned assistant replay on the follow-up. Anthropic's first correct answer was formatted as `**799**`; the numeric test now tolerates a Markdown wrapper without accepting arbitrary prose. The final signed-thinking replay passed.

Initial image/reasoning assertions exited `1` as described above. The targeted follow-up commands below all exited `0`.

## Reproduce the successful targeted checks

Build first, with keys in the local Git-ignored `.env`. The test runner reads only the three API-key assignments and does not execute the file.

```sh
cargo build --release --locked --offline
python3 -u tests/live.py --provider openai --model gpt-5.4-nano --features text
python3 -u tests/live.py --provider openai --model gpt-5.4-nano --features image,reasoning
python3 -u tests/live.py --provider anthropic --model claude-haiku-5-5 --features text
python3 -u tests/live.py --provider anthropic --model claude-haiku-5-5 --features reasoning
python3 -u tests/live.py --provider codex --model gpt-6.1-sol --features text
python3 -u tests/live.py --provider codex --model gpt-6.1-sol --features reasoning
python3 -u tests/live.py --provider openrouter --model nvidia/nemotron-3.5-super-vl:free --features text
python3 -u tests/live.py --provider openrouter --model nvidia/nemotron-3.5-super-vl:free --features reasoning
```

The larger `--features json,tools,image,reasoning` runs established the JSON/tool/image results for the other cases before encountering the assertion issues documented above. The OpenRouter larger run exited `0`. To rerun all checks with the updated fixtures, omit `--features`; expect seven requests per mode, with charges for API-key modes other than the verified free OpenRouter model.

Use `--list --provider openai`, `anthropic`, `openrouter`, or `codex` for catalog discovery without generation. OpenRouter generation is refused unless the selected catalog entry has a `:free` suffix and zero prompt/completion prices. Model availability can change.

### Subscription boundary

The crate does not discover or refresh subscription credentials. `tests/codex_session.py` is an explicit read-only bridge for these live tests. It reads `FX_HOME/chatgpt-auth.json` or `~/.fx/chatgpt-auth.json`, verifies a private user-owned regular file and non-expired version-1 session, and supplies `CODEX_ACCESS_TOKEN` and `CODEX_ACCOUNT_ID` only to the child environment. It does not print, persist, rotate, or refresh tokens, and no subscription credentials are copied into `.env` or repository files. If near expiry, refresh the login in fx first.

Codex rejects `max_output_tokens`; the client strips it even from native options. Consequently common `max_tokens` is not a subscription generation cap. The smoke tests use short prompts and low reasoning effort, subject to the server's model limits, request timeout, and client cancellation.

## Deterministic verification

All exit `0` after narrowing scope and adding subscription support:

```sh
cargo test --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
cargo build --release --locked --offline
```

There are 30 deterministic Rust tests, including subscription headers/body constraints, terminal status handling, finalized streamed-item recovery, replay, and local HTTP transport for all four modes. Formatted production Rust is 694 physical lines. Python live helpers use only the standard library and passed `py_compile`; no live calls run under `cargo test`.

The November 4 OpenRouter reasoning-content classification fix remains covered by `tests/responses_reasoning.rs`. Existing offline and live tests are evidence for their exercised cases, not a security audit or a guarantee of compatibility with every model.
