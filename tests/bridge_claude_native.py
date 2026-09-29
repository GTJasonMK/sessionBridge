"""Opt-in real Claude CLI/MCP test against a deterministic local Messages API.

No paid model calls. Requires the claude executable; validates native transport,
skill loading and handshake independently of upstream provider availability.
"""
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import tempfile
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from bridge_runtime import rpc, wait_until


class API(BaseHTTPRequestHandler):
    calls = 0

    def log_message(self, *_):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if "count_tokens" in self.path:
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"input_tokens":100}')
            return
        API.calls += 1
        names = [t["name"] for t in request.get("tools", [])]
        name = next((n for n in names if n.endswith("sessionbridge__sessionbridge")), None)
        previous = [c.get("input", {}).get("operation") for m in request.get("messages", []) for c in m.get("content", []) if isinstance(c, dict) and c.get("type") == "tool_use"]
        raw = json.dumps(request)
        challenge = re.search(r"challenge=([0-9a-f]+)", raw)
        generation = re.search(r"generation=(\d+)", raw)
        operation = next((op for op in ("skill.read", "inspect", "ready") if op not in previous), None)
        args = {"request_id": str(uuid.uuid4())}
        if operation == "ready":
            args.update(protocol="2", generation=int(generation[1]), challenge=challenge[1])
        tool = name and operation and challenge
        content = {"type": "tool_use", "id": "tool_" + uuid.uuid4().hex, "name": name, "input": {}} if tool else {"type": "text", "text": ""}
        events = [
            {"type": "message_start", "message": {"id": "msg_" + uuid.uuid4().hex, "type": "message", "role": "assistant", "model": request.get("model", "fixture"), "content": [], "stop_reason": None, "stop_sequence": None, "usage": {"input_tokens": 100, "output_tokens": 0}}},
            {"type": "content_block_start", "index": 0, "content_block": content},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": json.dumps({"operation": operation, "args": args})} if tool else {"type": "text_delta", "text": "连接正常"}},
            {"type": "content_block_stop", "index": 0},
            {"type": "message_delta", "delta": {"stop_reason": "tool_use" if tool else "end_turn", "stop_sequence": None}, "usage": {"output_tokens": 20}},
            {"type": "message_stop"},
        ]
        payload = "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def main():
    binary = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix="sb-native-") as temp:
        base = Path(temp)
        root, state = base / "project", base / "state"
        root.mkdir()
        for args in (("init", "-b", "main"), ("config", "user.name", "Test"), ("config", "user.email", "test@example.invalid"), ("commit", "--allow-empty", "-m", "baseline")):
            subprocess.run(["git", *args], cwd=root, check=True, capture_output=True)
        api = ThreadingHTTPServer(("127.0.0.1", 0), API)
        threading.Thread(target=api.serve_forever, daemon=True).start()
        dbpath = base / "providers.db"
        with sqlite3.connect(dbpath) as db:
            db.execute("CREATE TABLE providers(id,name,app_type,settings_config)")
            db.execute("INSERT INTO providers VALUES(?,?,?,?)", ("fixture", "Native test", "claude", json.dumps({"env": {"ANTHROPIC_API_KEY": "test-only", "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{api.server_port}", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "NO_PROXY": "127.0.0.1,localhost"}})))
        service = subprocess.Popen([str(binary), "serve"], env=dict(os.environ, SESSIONBRIDGE_V2_STATE=str(state), SESSIONBRIDGE_CCSWITCH_DB=str(dbpath)), stderr=subprocess.PIPE)
        try:
            wait_until(lambda: (state / "bridge.sock").exists())
            call = lambda op, args=None: rpc(state, root, op, args)
            room = call("room.create", {"title": "Native Claude transport"})["id"]
            member = next(m for m in call("room.get", {"room": room})["members"] if m["role"] == "coordinator")
            call("member.configure", {"member": member["id"], "cli": "claude", "provider": "fixture", "model": "claude-sonnet-4-6"})
            call("member.initialize", {"member": member["id"]})
            def joined():
                current = next(m for m in call("room.get", {"room": room})["members"] if m["id"] == member["id"])
                if current["state"] == "offline":
                    raise AssertionError((current["error"], API.calls))
                return current["state"] == "ready"
            wait_until(joined, 45)
            call("member.stop", {"member": member["id"]})
            assert API.calls >= 3
            print("PASS: real Claude CLI + local Messages API + real MCP READY; API calls:", API.calls)
        finally:
            service.terminate()
            service.communicate(timeout=10)
            api.shutdown()


if __name__ == "__main__":
    main()
