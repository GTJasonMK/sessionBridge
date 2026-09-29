"""Opt-in paid CLI smoke using the currently selected CC Switch providers.

python3 tests/bridge_live.py target/debug/sessionbridge
Creates an isolated Git repository, never modifies the user's project/config.
"""
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time
import tomllib
from bridge_runtime import rpc, wait_until


def main():
    binary = Path(sys.argv[1]).resolve()
    database = Path.home() / ".cc-switch/cc-switch.db"
    with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as db:
        providers = db.execute("SELECT app_type,id,name,settings_config FROM providers WHERE is_current=1 AND app_type IN ('claude','codex')").fetchall()
    assert {p[0] for p in providers} == {"claude", "codex"}, "Select one provider for each CLI in CC Switch"
    if len(sys.argv)>2:
        providers=[p for p in providers if p[0]==sys.argv[2]]
    directory = Path(tempfile.mkdtemp(prefix="sb-live-"))
    state, root = directory / "state", directory / "project"
    root.mkdir()
    def git(*args):
        subprocess.run(["git", *args], cwd=root, check=True, capture_output=True)
    git("init", "-b", "main")
    git("config", "user.name", "SessionBridge Smoke")
    git("config", "user.email", "smoke@example.invalid")
    (root / "README.md").write_text("A disposable SessionBridge smoke workspace. Do not modify files.\n")
    git("add", ".")
    git("commit", "-m", "baseline")
    service = subprocess.Popen([str(binary), "serve"], env=dict(os.environ, SESSIONBRIDGE_V2_STATE=str(state)), stderr=subprocess.PIPE)
    print("Artifacts:", directory, flush=True)
    try:
        wait_until(lambda: (state / "bridge.sock").exists())
        call = lambda op, args=None: rpc(state, root, op, args)
        room = call("room.create", {"title": "CLI 入群验证"})["id"]
        members = call("room.get", {"room": room})["members"]
        agents = {"claude": next(m for m in members if m["role"] == "coordinator"), "codex": next(m for m in members if m["role"] == "executor")}
        for cli, provider, name, raw in providers:
            config = json.loads(raw)
            model = (config.get("model") or config["env"].get("ANTHROPIC_MODEL")) if cli == "claude" else tomllib.loads(config["config"])["model"]
            print("Initializing:", cli, name, model, flush=True)
            call("member.configure", {"member": agents[cli]["id"], "cli": cli, "provider": provider, "model": model})
            call("member.initialize", {"member": agents[cli]["id"]})
        deadline = time.monotonic() + 135
        while time.monotonic() < deadline:
            snapshot = call("room.get", {"room": room})
            active = [m for m in snapshot["members"] if m["id"] in [agents[p[0]]["id"] for p in providers]]
            if any(m["state"] == "offline" for m in active):
                for m in active:
                    print(m["cli"], m["state"], m["error"], flush=True)
                raise AssertionError("live CLI initialization failed; inspect private runtime stderr")
            if all(m["state"] in ("ready", "busy") for m in active):
                break
            time.sleep(1)
        else:
            raise AssertionError("live initialization timed out")
        for agent in (agents.values() if len(providers)==2 else []):
            message = call("message.send", {"room": room, "kind": "request", "body": "仅测试群通信：请通过 message.send 向我回复‘连接正常’，确认处理本消息，然后结束本轮。不要创建工作项或修改文件。", "recipients": [agent["id"]]})
            wait_until(lambda: any(m["reply_to"] == message["id"] and m["sender"] == agent["id"] for m in call("history", {"room": room})["messages"]), 120)
        print("PASS: live handshake for", [p[0] for p in providers], "with directed replies" if len(providers)==2 else "(initialization only)", flush=True)
    finally:
        if (state / "bridge.sock").exists():
            try:
                for m in rpc(state, root, "room.get", {"room": room})["members"]:
                    if m["role"] != "boss":
                        rpc(state, root, "member.stop", {"member": m["id"]})
            except Exception as error:
                print("Cleanup:", error, file=sys.stderr)
        service.terminate()
        service.communicate(timeout=10)


if __name__ == "__main__":
    main()
