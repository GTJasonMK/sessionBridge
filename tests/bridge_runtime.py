"""Real daemon + native-protocol fixtures + real MCP, without model charges.

Run after cargo build: python3 tests/bridge_runtime.py target/debug/sessionbridge
"""
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import uuid
from concurrent.futures import ThreadPoolExecutor

FAKE_CLI = r'''#!/usr/bin/env python3
import json, os, pathlib, re, subprocess, sys, tomllib, uuid
codex = pathlib.Path(sys.argv[0]).name == 'codex'
directory = pathlib.Path(os.environ['CODEX_HOME' if codex else 'CLAUDE_CONFIG_DIR'])
if codex:
    config = tomllib.loads((directory / 'config.toml').read_text())['mcp_servers']['sessionbridge']
else:
    config = json.loads((directory / 'mcp.json').read_text())['mcpServers']['sessionbridge']
mcp = subprocess.Popen([config['command'], *config['args']], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
counter = 0
def tool(operation, args):
    global counter
    counter += 1
    args['request_id'] = str(uuid.uuid4())
    frame = {'jsonrpc':'2.0','id':counter,'method':'tools/call','params':{'name':'sessionbridge','arguments':{'operation':operation,'args':args}}}
    mcp.stdin.write(json.dumps(frame)+'\n'); mcp.stdin.flush()
    result = json.loads(mcp.stdout.readline())['result']
    if result.get('isError'): raise RuntimeError(result)
    return json.loads(result['content'][0]['text'])
def respond(value):
    print(json.dumps(value), flush=True)
def act(prompt):
    if prompt.startswith('JOIN'):
        tool('skill.read', {})
        tool('inspect', {})
        tool('ready', {'protocol':'2','generation':int(re.search(r'generation=(\d+)', prompt)[1]),'challenge':re.search(r'challenge=(\w+)', prompt)[1]})
    else:
        messages=json.loads(prompt.rsplit('\n',1)[1])
        identity=tool('inspect', {})['member']
        for message in messages:
            if message['sender']!='system' and message['kind']=='request':
                tool('message.send', {'room':identity['room_id'],'kind':'reply','body':'已收到：'+message['body'],'recipients':[message['sender']],'reply_to':message['id']})
        tool('message.ack', {'messages':[m['id'] for m in messages]})
for line in sys.stdin:
    frame=json.loads(line)
    if codex:
        method=frame.get('method')
        if method=='initialize': respond({'id':frame['id'],'result':{'userAgent':'fixture'}})
        elif method=='thread/resume' and os.environ.get('FIXTURE_FAIL_RESUME') and not (directory / 'resume.failed').exists():
            (directory / 'resume.failed').write_text('1')
            respond({'id':frame['id'],'error':{'code':'THREAD_NOT_FOUND','message':'fixture stale thread'}})
        elif method in ('thread/start','thread/resume'): respond({'id':frame['id'],'result':{'thread':{'id':'fixture-thread'}}})
        elif method=='turn/start':
            respond({'id':frame['id'],'result':{'turn':{'id':'fixture-turn'}}})
            act(frame['params']['input'][0]['text'])
            respond({'method':'item/agentMessage/delta','params':{'delta':'Fixture complete'}})
            respond({'method':'turn/completed','params':{'turn':{'status':'completed'}}})
    elif frame.get('type')=='user':
        respond({'type':'system','subtype':'init','session_id':'fixture-session'})
        act(frame['message']['content'])
        respond({'type':'assistant','message':{'content':[{'type':'text','text':'Fixture complete'}]}})
        respond({'type':'result','is_error':False})
'''


def rpc(state, workspace, operation, args=None, token=None):
    args = dict(args or {})
    args.setdefault("request_id", str(uuid.uuid4()))
    token = token or (state / "operator.token").read_text()
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(70)
        client.connect(str(state / "bridge.sock"))
        client.sendall((json.dumps({"workspace": str(workspace), "operation": operation, "args": args, "token": token}) + "\n").encode())
        response = json.loads(client.makefile().readline())
    if not response["ok"]:
        raise RuntimeError(response["error"])
    return response["data"]


def wait_until(check, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError("condition did not become true before timeout")


def main():
    binary = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix="sb-runtime-") as temporary:
        directory = Path(temporary)
        state, root, binaries = [directory / name for name in ("state", "project", "bin")]
        for path in (state, root, binaries):
            path.mkdir()
        for cli in ("claude", "codex"):
            path = binaries / cli
            path.write_text(FAKE_CLI)
            path.chmod(0o700)
        dbpath = directory / "providers.db"
        with sqlite3.connect(dbpath) as db:
            db.execute("CREATE TABLE providers(id TEXT,name TEXT,app_type TEXT,settings_config TEXT)")
            db.execute("INSERT INTO providers VALUES(?,?,?,?)", ("c", "Claude fixture", "claude", json.dumps({"env": {"ANTHROPIC_API_KEY": "fixture-only"}})))
            db.execute("INSERT INTO providers VALUES(?,?,?,?)", ("o", "Codex fixture", "codex", json.dumps({"config": 'model="fixture"', "auth": {"OPENAI_API_KEY": "fixture-only"}})))
        def git(*args):
            return subprocess.run(["git", *args], cwd=root, check=True, capture_output=True)
        git("init", "-b", "main")
        git("config", "user.name", "Test")
        git("config", "user.email", "test@example.invalid")
        (root / "README.md").write_text("baseline\n")
        git("add", ".")
        git("commit", "-m", "baseline")
        environment = dict(os.environ, SESSIONBRIDGE_V2_STATE=str(state), SESSIONBRIDGE_CCSWITCH_DB=str(dbpath), FIXTURE_FAIL_RESUME="1", PATH=str(binaries) + os.pathsep + os.environ["PATH"])
        service = subprocess.Popen([str(binary), "serve"], env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        try:
            wait_until(lambda: (state / "bridge.sock").exists())
            call = lambda op, args=None: rpc(state, root, op, args)
            room = call("room.create", {"title": "真实进程协议验证"})["id"]
            members = call("room.get", {"room": room})["members"]
            coordinator = next(m for m in members if m["role"] == "coordinator")
            executor = next(m for m in members if m["role"] == "executor")
            for member, cli, provider in [(coordinator, "claude", "c"), (executor, "codex", "o")]:
                call("member.configure", {"member": member["id"], "cli": cli, "provider": provider, "model": "fixture"})
                initialize = {"member": member["id"], "request_id": str(uuid.uuid4())}
                first = call("member.initialize", initialize)
                assert call("member.initialize", initialize) == first
            wait_until(lambda: all(m["state"] in ("ready", "busy") for m in call("room.get", {"room": room})["members"]))
            message = call("message.send", {"room": room, "kind": "request", "body": "验证单播", "recipients": [executor["id"]]})
            wait_until(lambda: any(m["reply_to"] == message["id"] for m in call("history", {"room": room})["messages"]))
            history = call("history", {"room": room})
            deliveries = [d for d in history["deliveries"] if d["message_id"] == message["id"]]
            assert len(deliveries) == 1 and deliveries[0]["member_id"] == executor["id"]
            wait_until(lambda: next(d for d in call("history", {"room": room})["deliveries"] if d["message_id"] == message["id"])["state"] == "processed")
            call("member.stop", {"member": executor["id"]})
            wait_until(lambda: next(m for m in call("room.get", {"room": room})["members"] if m["id"] == executor["id"])["state"] == "offline")
            call("member.initialize", {"member": executor["id"]})
            wait_until(lambda: next(m for m in call("room.get", {"room": room})["members"] if m["id"] == executor["id"])["state"] in ("ready", "busy"))
            extra=call("member.add", {"room":room,"name":"Race executor"})
            call("member.configure", {"member":extra["id"],"cli":"codex","provider":"o","model":"fixture"})
            with ThreadPoolExecutor(max_workers=2) as pool:
                initializing=pool.submit(call,"member.initialize",{"member":extra["id"]})
                paused=pool.submit(call,"room.transition",{"room":room,"state":"paused"})
                assert paused.result()["state"]=="paused"
                try:
                    initialized = initializing.result()
                except RuntimeError as error:
                    assert error.args[0]["code"]=="ROOM_INACTIVE", error
                else:
                    assert initialized["state"] == "starting"
            stopped=call("room.get",{"room":room})
            assert all(m["state"]=="offline" for m in stopped["members"] if m["role"]!="boss"), stopped
            with sqlite3.connect(next((state/"workspaces").glob("*/state.db"))) as db:
                assert db.execute("SELECT count(*) FROM processes").fetchone()[0]==0
            call("member.remove",{"member":extra["id"]})
            call("room.transition",{"room":room,"state":"active"})
            for member in (coordinator,executor):
                call("member.initialize",{"member":member["id"]})
            wait_until(lambda: all(m["state"] in ("ready","busy","removed") for m in call("room.get",{"room":room})["members"]))
            assert not (root / ".sessionbridge").exists()
            call("service.shutdown")
            service.wait(timeout=10)
            print("PASS: daemon, native frames, MCP, ACK, concurrent initialization/pause, remove/resume and shutdown")
        finally:
            service.terminate()
            _, errors = service.communicate(timeout=10)
            if errors:
                print(errors.decode(), file=sys.stderr)


if __name__ == "__main__":
    main()
