#!/usr/bin/env python3
"""What Claude Code's control protocol will actually let a host do.

Not a test — it needs a logged-in Claude Code and it spends quota, so it is not
in `checks.toml`. It is the evidence behind `docs/ARCHITECTURE.md`, kept here
because the first version of it was written in a scratch directory, proved the
thing the architecture rests on, and was gone a day later.

    python3 docs/probes/control_protocol.py all

Each experiment prints PROVED or FAILED against a property that can actually be
wrong. `deny` and `rewrite` are the ones that matter: an approval path that
cannot refuse, or cannot alter what it approves, is decoration.

Verified against Claude Code 2.1.241 on a Claude Max subscription, 2026-08-24.
No SDK, no API key, no Node — plain JSON over two pipes.
"""
import json, os, subprocess, sys, tempfile, threading, time

CLI = ["claude", "-p", "--input-format", "stream-json",
       "--output-format", "stream-json", "--verbose"]


class Host:
    """A host that serves one MCP tool: the one Claude Code asks for permission."""

    def __init__(self, work, decide=None, extra=(), tools=None, on_call=None):
        self.work, self.decide = work, decide
        self.tools, self.on_call = tools, on_call
        self.asked, self.init, self.tool_started, self.result = [], None, None, None
        self.served, self.calls = False, []
        cmd = CLI + list(extra)
        if decide:
            # The seam `claude --help` does not mention.
            cmd += ["--permission-prompt-tool", "mcp__host__approve"]
        self.p = subprocess.Popen(cmd, cwd=work, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  text=True, bufsize=1)
        self.done = threading.Event()
        threading.Thread(target=self._pump, daemon=True).start()

    def send(self, obj):
        self.p.stdin.write(json.dumps(obj) + "\n")
        self.p.stdin.flush()

    def _ok(self, rid, payload):
        self.send({"type": "control_response",
                   "response": {"subtype": "success", "request_id": rid,
                                "response": payload}})

    def _pump(self):
        for line in self.p.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                m = json.loads(line)
            except json.JSONDecodeError:
                continue
            t = m.get("type")

            if t == "control_response":
                r = m.get("response", {}).get("response")
                if isinstance(r, dict) and "models" in r:
                    self.init = r

            elif t == "control_request":
                rid, req = m["request_id"], m["request"]
                if req.get("subtype") == "mcp_message" and (self.decide or self.tools):
                    self._serve(rid, req.get("message", {}))
                else:
                    # Every control_request needs a response. A missed one is a
                    # session that stalls with no error, which cost an evening.
                    self._ok(rid, {})

            elif t == "assistant":
                for c in m["message"].get("content", []):
                    if c.get("type") == "tool_use" and self.tool_started is None:
                        self.tool_started = time.time()

            elif t == "result":
                self.result = m
                self.done.set()

    def _serve(self, rid, inner):
        method, mid = inner.get("method"), inner.get("id")
        def rpc(result):
            self._ok(rid, {"mcp_response": {"jsonrpc": "2.0", "id": mid,
                                            "result": result}})
        if method == "initialize":
            rpc({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}},
                 "serverInfo": {"name": "host", "version": "1.0.0"}})
        elif method == "tools/list":
            self.served = True
            rpc({"tools": self.tools or [{
                "name": "approve",
                "description": "Decide whether a tool call may proceed",
                "inputSchema": {"type": "object", "properties": {
                    "tool_name": {"type": "string"}, "input": {"type": "object"}},
                    "required": ["tool_name", "input"]}}]})
        elif method == "tools/call":
            args = inner.get("params", {}).get("arguments", {})
            if self.tools:
                # A tool the model chose to call, not a permission decision.
                self.calls.append(args)
                rpc({"content": [{"type": "text", "text": self.on_call(args)}]})
            else:
                name, tin = args.get("tool_name"), args.get("input", {})
                self.asked.append((name, tin))
                rpc({"content": [{"type": "text",
                                  "text": json.dumps(self.decide(name, tin))}]})
        else:
            rpc({}) if mid is not None else self._ok(rid, {})

    def start(self, servers=None):
        req = {"subtype": "initialize"}
        if servers:
            req["sdkMcpServers"] = servers
        self.send({"type": "control_request", "request_id": "init", "request": req})
        time.sleep(0.5)

    def ask(self, text):
        self.send({"type": "user",
                   "message": {"role": "user", "content": text}})

    def stop(self):
        self.p.terminate()


def say(name, passed, detail):
    print(f"  {'PROVED ' if passed else 'FAILED '} {name}: {detail}")
    return passed


def probe_allow(work):
    """The host is consulted before a tool runs, and can let it through."""
    f = os.path.join(work, "allowed.txt")
    h = Host(work, lambda n, i: {"behavior": "allow", "updatedInput": i})
    h.start(["host"])
    h.ask(f"Run exactly this bash command and nothing else: touch {f}")
    h.done.wait(180); h.stop()
    return say("allow", h.asked and os.path.exists(f),
               f"{len(h.asked)} request(s) reached the host; file created={os.path.exists(f)}")


def probe_deny(work):
    """The refutation. If deny does not stop it, none of this is a leash."""
    f = os.path.join(work, "denied.txt")
    h = Host(work, lambda n, i: {"behavior": "deny", "message": "the host said no"})
    h.start(["host"])
    h.ask(f"Run exactly this bash command and nothing else: touch {f}")
    h.done.wait(180); h.stop()
    return say("deny", h.asked and not os.path.exists(f),
               f"{len(h.asked)} request(s) reached the host; file created={os.path.exists(f)}")


def probe_rewrite(work):
    """The host can alter a call before it happens, not only judge it."""
    asked_f = os.path.join(work, "asked.txt")
    got_f = os.path.join(work, "rewritten.txt")
    def decide(n, i):
        out = dict(i)
        out["command"] = i.get("command", "").replace("asked.txt", "rewritten.txt")
        return {"behavior": "allow", "updatedInput": out}
    h = Host(work, decide)
    h.start(["host"])
    h.ask(f"Run exactly this bash command and nothing else: touch {asked_f}")
    h.done.wait(180); h.stop()
    ok = os.path.exists(got_f) and not os.path.exists(asked_f)
    return say("rewrite", ok,
               f"asked-for file={os.path.exists(asked_f)}, rewritten file={os.path.exists(got_f)}")


def probe_capabilities(work):
    """What the subscription can do, discovered rather than hardcoded."""
    h = Host(work)
    h.start()
    for _ in range(40):
        if h.init:
            break
        time.sleep(0.25)
    h.stop()
    r = h.init or {}
    acct = r.get("account", {})
    models = r.get("models", [])
    ok = bool(models and acct.get("subscriptionType"))
    if ok:
        efforts = models[0].get("supportedEffortLevels")
        print(f"           {len(models)} models, efforts={efforts}, "
              f"{len(r.get('agents') or [])} agents, "
              f"{len(r.get('commands') or [])} commands, "
              f"tier={acct.get('subscriptionType')}, mode={r.get('current_permission_mode')}")
    return say("capabilities", ok, f"initialize returned {len(r)} fields")


def probe_levers(work):
    """Interrupt a running turn, and change permissions without restarting."""
    h = Host(work, extra=["--permission-mode", "bypassPermissions"])
    h.start()
    h.send({"type": "control_request", "request_id": "m1",
            "request": {"subtype": "set_permission_mode", "mode": "plan"}})
    time.sleep(0.5)
    h.send({"type": "control_request", "request_id": "m2",
            "request": {"subtype": "set_permission_mode", "mode": "bypassPermissions"}})
    time.sleep(0.5)
    h.ask("Run this exact bash command: sleep 90 && echo finished")
    t0 = time.time()
    while h.tool_started is None and time.time() - t0 < 60:
        time.sleep(0.2)
    if h.tool_started is None:
        h.stop(); return say("levers", False, "the tool never started")
    time.sleep(3)
    sent = time.time()
    h.send({"type": "control_request", "request_id": "int",
            "request": {"subtype": "interrupt"}})
    h.done.wait(60); h.stop()
    took = (time.time() - sent) if h.result else None
    ok = h.result is not None and took < 30
    return say("levers", ok,
               f"turn ended {took:.1f}s after interrupt, with ~87s of sleep left"
               if ok else "no result within 60s of the interrupt")


def probe_kernel_tools(work):
    """The kernel can serve ordinary tools, not only the permission tool.

    This is what lets a supervisor's only way to create work be a call into the
    kernel: no socket, no CLI on its PATH, and nothing that happens without the
    kernel executing it.
    """
    tool = {"name": "remember",
            "description": "Store a note in Ironsight's kernel. Use this when asked to remember something.",
            "inputSchema": {"type": "object",
                            "properties": {"note": {"type": "string"}},
                            "required": ["note"]}}
    cfg = json.dumps({"mcpServers": {"host": {"type": "sdk", "name": "host"}}})
    h = Host(work, tools=[tool], on_call=lambda a: "stored as note #1",
             extra=["--permission-mode", "bypassPermissions", "--mcp-config", cfg])
    h.start(["host"])
    h.ask("Use the remember tool to store the note 'the kernel is reachable'. "
          "Use only that tool.")
    h.done.wait(180); h.stop()
    return say("kernel-tools", bool(h.calls),
               f"tools/list served={h.served}, calls from the model={len(h.calls)}")


PROBES = {"allow": probe_allow, "deny": probe_deny, "rewrite": probe_rewrite,
          "capabilities": probe_capabilities, "levers": probe_levers,
          "kernel-tools": probe_kernel_tools}

if __name__ == "__main__":
    which = sys.argv[1:] or ["all"]
    names = list(PROBES) if which == ["all"] else which
    work = tempfile.mkdtemp(prefix="ironsight-probe-")
    print(f"working in {work}\n")
    results = [PROBES[n](work) for n in names]
    print(f"\n{sum(1 for r in results if r)}/{len(results)} proved")
    sys.exit(0 if all(results) else 1)
