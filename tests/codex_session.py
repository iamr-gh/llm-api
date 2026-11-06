"""Read-only bridge for explicit live tests of the user's existing fx login."""
import json
import os
from pathlib import Path
import stat
import time
import urllib.request


def environment(env):
    path = Path(env.get("FX_HOME", Path.home() / ".fx")) / "chatgpt-auth.json"
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd) as file:
        info = os.fstat(file.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise RuntimeError("fx auth file must be a private, user-owned regular file")
        session = json.loads(file.read(1024 * 1024))
    if session.get("version") != 1:
        raise RuntimeError("unsupported fx auth schema")
    if session.get("expires_at_ms", 0) < (time.time() + 60) * 1000:
        raise RuntimeError("fx access token expired or near expiry; refresh the login in fx first")
    if not all(isinstance(session.get(k), str) and session[k] for k in ("access_token", "account_id")):
        raise RuntimeError("fx session lacks access token or account ID")
    return {**env, "CODEX_ACCESS_TOKEN": session["access_token"], "CODEX_ACCOUNT_ID": session["account_id"]}


def models(env):
    env = environment(env)
    with urllib.request.urlopen("https://registry.npmjs.org/@openai/codex/latest", timeout=30) as response:
        version = json.load(response)["version"]
    request = urllib.request.Request(
        "https://chatgpt.com/backend-api/codex/models?client_version=" + version,
        headers={"Authorization": "Bearer " + env["CODEX_ACCESS_TOKEN"],
                 "chatgpt-account-id": env["CODEX_ACCOUNT_ID"],
                 "originator": "fx", "Accept": "application/json"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)["models"]
