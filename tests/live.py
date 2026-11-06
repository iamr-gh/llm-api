"""Opt-in live checks. Uses synthetic prompts; never prints keys or raw replay."""
import argparse
import base64
import json
import os
from pathlib import Path
import struct
import subprocess
import urllib.error
import urllib.request
import zlib
import codex_session

ROOT = Path(__file__).resolve().parents[1]


def environment():
    env = os.environ.copy()
    for line in (ROOT / ".env").read_text().splitlines():
        key, separator, value = line.strip().removeprefix("export ").partition("=")
        if separator and key.strip() in ("OPENAI_API_KEY", "OPENROUTER_API_KEY", "ANTHROPIC_API_KEY"):
            env[key.strip()] = value.strip().strip("\"'")
    return env


def models(provider, env):
    if provider == "codex":
        return codex_session.models(env)
    host, key = {
        "openai": ("https://api.openai.com/v1/models", "OPENAI_API_KEY"),
        "openrouter": ("https://openrouter.ai/api/v1/models", "OPENROUTER_API_KEY"),
        "anthropic": ("https://api.anthropic.com/v1/models", "ANTHROPIC_API_KEY"),
    }[provider]
    headers = ({"x-api-key": env[key], "anthropic-version": "2023-06-01"}
               if provider == "anthropic" else {"Authorization": "Bearer " + env[key]})
    request = urllib.request.Request(host, headers=headers)
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)["data"]


def free(model):
    prices = model.get("pricing", {})
    return model["id"].endswith(":free") and all(
        float(prices.get(field, "inf")) == 0 for field in ("prompt", "completion")
    )


def catalog(env, selected=None):
    for provider in ([selected] if selected else ["openai", "anthropic", "openrouter"]):
        entries = models(provider, env)
        for entry in sorted(entries, key=lambda m: m.get("created", 0), reverse=True):
            if provider == "openrouter" and not free(entry):
                continue
            if provider == "openai" and not any(part in entry["id"] for part in ("nano", "mini")):
                continue
            if provider == "codex" and entry.get("visibility") != "list":
                continue
            print(json.dumps({"provider": provider, "id": entry.get("id", entry.get("slug")),
                              "pricing": entry.get("pricing"),
                              "modalities": entry.get("architecture", {}).get("input_modalities"),
                              "parameters": entry.get("supported_parameters")}))


class Session:
    def __init__(self, env, provider, model):
        self.provider, self.model = provider, model
        self.adaptive_thinking = False
        if provider == "anthropic":
            info = next(m for m in models(provider, env) if m["id"] == model)
            self.adaptive_thinking = info["capabilities"]["thinking"]["types"]["adaptive"]["supported"]
        self.process = subprocess.Popen([str(ROOT / "target/release/llm-api")], env=env,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, text=True, bufsize=1)
        self.input_tokens = self.output_tokens = 0
        self.reported_cost = 0.0
        self.cost_reports = 0

    def call(self, name, messages, **options):
        request = {"model": self.model, "messages": messages, "max_tokens": 256, **options}
        if self.provider == "codex":
            request.setdefault("options", {}).setdefault("reasoning", {"effort": "low", "summary": "auto"})
        command = {"provider": self.provider, "request": request}
        self.process.stdin.write(json.dumps(command) + "\n")
        self.process.stdin.flush()
        events = []
        for line in self.process.stdout:
            event = json.loads(line)
            if event["type"] == "error":
                raise RuntimeError(event["message"])
            if event["type"] == "done":
                response = event["response"]
                usage = response["usage"]
                self.input_tokens += usage["normalized"].get("input_tokens") or 0
                self.output_tokens += usage["normalized"].get("output_tokens") or 0
                cost = (usage.get("native") or {}).get("cost")
                if cost is not None:
                    self.reported_cost += float(cost)
                    self.cost_reports += 1
                print(json.dumps({"provider": self.provider, "model": self.model, "test": name,
                                  "finish_reason": response["finish_reason"], "events": len(events),
                                  "usage": usage["normalized"], "native_cost": cost}))
                return response, events
            events.append(event)
        raise RuntimeError("CLI exited before a terminal event")

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=10)
        self.process.stdout.close()
        self.process.stderr.close()
        print(json.dumps({"provider": self.provider, "total_input_tokens": self.input_tokens,
                          "total_output_tokens": self.output_tokens,
                          "reported_cost": self.reported_cost if self.cost_reports else None}))


def message(text):
    return {"role": "user", "content": [{"type": "text", "text": text}]}


def text(response):
    return "".join(block["text"] for block in response["message"]["content"] if block["type"] == "text")


def image():
    def chunk(kind, payload):
        return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload))
    raw = (b"\x00" + b"\xff\x00\x00" * 256) * 256
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 256, 256, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
    return {"type": "image", "mime_type": "image/png", "data": base64.b64encode(png).decode()}


def suite(session, features):
    passed = []
    if "text" in features:
        response, events = session.call("text_stream", [message("Reply with exactly LIVE_OK.")])
        assert text(response).strip() == "LIVE_OK", "unexpected text response"
        assert any(event["type"] == "text_delta" for event in events), "no text deltas"
        passed.append("text_stream")
    if "json" in features:
        schema = {"type": "object", "properties": {"ok": {"type": "boolean"}},
                  "required": ["ok"], "additionalProperties": False}
        response, schema_events = session.call("json_schema", [message("Return a JSON object with ok set to true.")],
                                   json_schema=schema)
        answer = text(response)
        try:
            parsed = json.loads(answer)
        except json.JSONDecodeError:
            print(json.dumps({"test": "json_schema_diagnostic", "answer_excerpt": answer[:300],
                              "content_types": [b.get("type") for b in response["message"]["content"]],
                              "native_item_types": [b.get("type") for b in response["message"]["replay"]["data"]],
                              "text_delta_excerpt": "".join(e.get("delta", "") for e in schema_events if e["type"] == "text_delta")[:300]}))
            raise
        assert parsed == {"ok": True}, "schema response mismatch"
        passed.append("json_schema")
    if "tools" in features:
        tools = [{"name": "add", "description": "Add two integers.", "parameters": {
            "type": "object", "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}},
            "required": ["a", "b"], "additionalProperties": False}}]
        history = [message("Use the add tool for 2 + 3. After the result, reply with exactly 5.")]
        choice = {"type": "tool" if session.provider == "anthropic" else "function", "name": "add"}
        response, events = session.call("tool_call", history, tools=tools,
                                        options={"tool_choice": choice})
        calls = [block for block in response["message"]["content"] if block["type"] == "tool_call"]
        assert response["finish_reason"] == "tool_calls" and len(calls) == 1, "expected one tool call"
        call = calls[0]
        assert call["name"] == "add" and call["arguments"] == {"a": 2, "b": 3}, "wrong tool call"
        assert any(event["type"] == "tool_arguments_delta" for event in events), "no argument deltas"
        history += [response["message"], {"role": "user", "content": [
            {"type": "tool_result", "id": call["id"], "output": "5"}]}]
        response, _ = session.call("tool_replay", history, tools=tools,
                                   options={"tool_choice": {"type": "none"} if session.provider == "anthropic" else "none"})
        assert text(response).strip() == "5", "tool replay response mismatch"
        passed.append("tool_round_trip")
    if "image" in features:
        prompt = message("What is the solid color in this image? Answer with one color word.")
        prompt["content"].append(image())
        response, _ = session.call("image", [prompt])
        assert "red" in text(response).lower(), "image color mismatch: " + text(response)[:120]
        passed.append("image")
    if "reasoning" in features:
        anthropic = session.provider == "anthropic"
        options = ({"thinking": {"type": "enabled", "budget_tokens": 1024}}
                   if anthropic else {"reasoning": {"effort": "low"}})
        if anthropic and session.adaptive_thinking:
            options = {"thinking": {"type": "adaptive"}, "output_config": {"effort": "medium"}}
        if session.provider == "codex":
            options["reasoning"]["summary"] = "auto"
        limit = 2048 if session.provider == "openrouter" else 1536 if anthropic else 1024
        question = ("How many integers n from 1 through 1000000 satisfy n mod 7 = 3, "
                    "n mod 11 = 5, and n mod 13 = 7, but are not divisible by 5? "
                    "Return only the count as an integer.")
        expected = sum(n % 7 == 3 and n % 11 == 5 and n % 13 == 7 and n % 5 != 0
                       for n in range(1, 1000001))
        response, _ = session.call("reasoning", [message(question)], max_tokens=limit, options=options)
        assert int(text(response).strip().strip("*`")) == expected, "reasoning answer mismatch: " + text(response)[:120]
        native = response["message"]["replay"]["data"]
        blocks = native["content"] if anthropic else native
        has_reasoning = any(item.get("type") == ("thinking" if anthropic else "reasoning") for item in blocks)
        print(json.dumps({"provider": session.provider, "native_reasoning_present": has_reasoning}))
        assert has_reasoning, "no native reasoning item"
        history = [message(question), response["message"],
                   message("Add 1 to that answer. Reply with just the answer.")]
        response, _ = session.call("reasoning_replay", history, max_tokens=limit, options=options)
        assert int(text(response).strip().strip("*`")) == expected + 1, "reasoning replay mismatch"
        passed.append("reasoning_round_trip")
    print(json.dumps({"provider": session.provider, "passed": passed}))


def diagnose(env, provider, model):
    host, variable = {
        "openai": ("https://api.openai.com/v1/responses", "OPENAI_API_KEY"),
        "openrouter": ("https://openrouter.ai/api/v1/responses", "OPENROUTER_API_KEY"),
        "anthropic": ("https://api.anthropic.com/v1/messages", "ANTHROPIC_API_KEY"),
        "codex": ("https://chatgpt.com/backend-api/codex/responses", "CODEX_ACCESS_TOKEN"),
    }[provider]
    body = {"model": model, "input": [{"role": "user", "content": [
        {"type": "input_text", "text": "Reply with exactly LIVE_OK."}]}],
        "instructions": "", "tools": [], "max_output_tokens": 256,
        "stream": True, "store": False, "include": ["reasoning.encrypted_content"]}
    headers = {"Authorization": "Bearer " + env[variable], "Content-Type": "application/json"}
    if provider == "anthropic":
        headers = {"x-api-key": env[variable], "anthropic-version": "2023-06-01", "Content-Type": "application/json"}
        body = {"model": model, "messages": [{"role": "user", "content": "Reply with exactly LIVE_OK."}],
                "max_tokens": 256, "stream": True}
    if provider == "codex":
        headers.update({"chatgpt-account-id": env["CODEX_ACCOUNT_ID"], "originator": "llm-api",
                        "OpenAI-Beta": "responses=experimental", "Accept": "text/event-stream"})
        body.pop("max_output_tokens")
        body.update({"instructions": "You are a helpful assistant.", "reasoning": {"effort": "low"}})
    request = urllib.request.Request(host, data=json.dumps(body).encode(), headers=headers)
    kinds = []
    with urllib.request.urlopen(request, timeout=30) as response:
        for line in response:
            if not line.startswith(b"data:"):
                continue
            payload = line[5:].strip()
            if payload == b"[DONE]":
                continue
            event = json.loads(payload)
            kinds.append(event.get("type"))
            if "error" in event or event.get("type") in ("error", "response.failed"):
                error = event.get("error") or event.get("response", {}).get("error")
                safe = json.dumps({"event": event.get("type"), "error": error,
                                   "top_level_keys": list(event)})
                for variable in ("OPENAI_API_KEY", "OPENROUTER_API_KEY", "ANTHROPIC_API_KEY", "CODEX_ACCESS_TOKEN", "CODEX_ACCOUNT_ID"):
                    if env.get(variable):
                        safe = safe.replace(env[variable], "[REDACTED]")
                print(safe[:1500])
    print(json.dumps({"provider": provider, "event_types": sorted(set(kinds))}))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--list", action="store_true")
    parser.add_argument("--diagnose", action="store_true")
    parser.add_argument("--provider", choices=["openai", "anthropic", "openrouter", "codex"])
    parser.add_argument("--model")
    parser.add_argument("--features", default="text,json,tools,image,reasoning")
    args = parser.parse_args()
    env = environment()
    if args.list:
        catalog(env, args.provider)
        return
    if not args.provider or not args.model:
        parser.error("--provider and --model are required without --list")
    if args.provider == "openrouter":
        matches = [entry for entry in models("openrouter", env) if entry["id"] == args.model]
        if len(matches) != 1 or not free(matches[0]):
            raise RuntimeError("refusing OpenRouter model without verified zero prompt/completion pricing and :free suffix")
    if args.provider == "codex":
        env = codex_session.environment(env)
    if args.diagnose:
        diagnose(env, args.provider, args.model)
        return
    session = Session(env, args.provider, args.model)
    try:
        suite(session, args.features.split(","))
    finally:
        session.close()


if __name__ == "__main__":
    try:
        main()
    except urllib.error.HTTPError as error:
        print(json.dumps({"error": "model catalog HTTP failure", "status": error.code}))
        raise SystemExit(1)
    except Exception as error:
        print(json.dumps({"error": str(error)}))
        raise SystemExit(1)
