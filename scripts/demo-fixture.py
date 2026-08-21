#!/usr/bin/env python3
"""Build a synthetic ~/.claude for demos and manual testing.

Writes transcripts that look like real Claude Code sessions — prompts, tool
calls, patches, todos, a subagent, an error, usage — so every pane has
something to show without exposing anyone's actual work.

usage: demo-fixture.py <dir> [repo-path]
"""
import json, os, sys, time, uuid
from datetime import datetime, timedelta, timezone

OUT = sys.argv[1] if len(sys.argv) > 1 else "/tmp/ironsight-fixture"
REPO = sys.argv[2] if len(sys.argv) > 2 else "/tmp/ironsight-demo-repo"
now = datetime.now(timezone.utc)
lines_written = {}


def ts(seconds_ago):
    return (now - timedelta(seconds=seconds_ago)).isoformat().replace("+00:00", "Z")


def usage(inp=3, out=180, read=48000, write=6200):
    return {
        "input_tokens": inp,
        "output_tokens": out,
        "cache_read_input_tokens": read,
        "cache_creation_input_tokens": write,
        "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": write},
        "service_tier": "standard",
    }


def base(sid, cwd, when, branch="main"):
    return {"sessionId": sid, "cwd": cwd, "gitBranch": branch, "version": "2.1.234",
            "userType": "external", "entrypoint": "cli", "isSidechain": False,
            "parentUuid": None, "uuid": str(uuid.uuid4()), "timestamp": when}


def prompt(sid, cwd, when, text):
    r = base(sid, cwd, when)
    r.update(type="user", message={"role": "user", "content": text})
    return r


def say(sid, cwd, when, text):
    r = base(sid, cwd, when)
    r.update(type="assistant", effort="high",
             message={"model": "claude-opus-5", "role": "assistant", "usage": usage(),
                      "content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": text}]})
    return r


def call(sid, cwd, when, tool, tool_input, tid):
    r = base(sid, cwd, when)
    r.update(type="assistant", effort="high",
             message={"model": "claude-opus-5", "role": "assistant", "usage": usage(out=90),
                      "content": [{"type": "tool_use", "id": tid, "name": tool, "input": tool_input}]})
    return r


def result(sid, cwd, when, tid, payload, is_error=False, text="ok"):
    r = base(sid, cwd, when)
    r.update(type="user", toolUseResult=payload, sourceToolAssistantUUID=str(uuid.uuid4()),
             message={"role": "user", "content": [
                 {"type": "tool_result", "tool_use_id": tid, "is_error": is_error, "content": text}]})
    return r


def patch(path, old_start, added, removed):
    lines = [f"+{l}" for l in added] + [f"-{l}" for l in removed]
    return {"filePath": path, "structuredPatch": [
        {"oldStart": old_start, "oldLines": len(removed), "newStart": old_start,
         "newLines": len(added), "lines": lines}]}


def write(slug, sid, records):
    d = os.path.join(OUT, "projects", slug)
    os.makedirs(d, exist_ok=True)
    path = os.path.join(d, f"{sid}.jsonl")
    with open(path, "w") as f:
        for r in records:
            f.write(json.dumps(r) + "\n")
    lines_written[slug] = len(records)
    return path


# ── an active session: edits, a subagent, a failing command, a plan ──────────
api = str(uuid.uuid4())
cwd = REPO
recs = [
    {"type": "ai-title", "sessionId": api, "aiTitle": "Rate limit the public API"},
    prompt(api, cwd, ts(900), "Add a rate limiter to the public API and cover it with tests."),
    say(api, cwd, ts(880), "I'll add a token bucket in the middleware layer, then test it."),
    call(api, cwd, ts(870), "TodoWrite", {"todos": []}, "t1"),
    result(api, cwd, ts(869), "t1", {"newTodos": [
        {"content": "Add a token bucket limiter", "status": "completed"},
        {"content": "Wire it into the middleware chain", "status": "in_progress"},
        {"content": "Cover burst and refill in tests", "status": "pending"},
        {"content": "Document the headers it sets", "status": "pending"}]}),
    call(api, cwd, ts(840), "Read", {"file_path": f"{cwd}/src/middleware.rs"}, "t2"),
    result(api, cwd, ts(839), "t2", {"type": "text", "file": {
        "filePath": f"{cwd}/src/middleware.rs", "numLines": 84, "totalLines": 84, "content": "…"}}),
    call(api, cwd, ts(820), "Write", {"file_path": f"{cwd}/src/limiter.rs", "content": "x" * 2400}, "t3"),
    result(api, cwd, ts(818), "t3", patch(f"{cwd}/src/limiter.rs", 1,
           ["pub struct Limiter {", "    capacity: u32,", "    refill: Duration,", "}"], [])),
    call(api, cwd, ts(800), "Edit", {"file_path": f"{cwd}/src/middleware.rs"}, "t4"),
    result(api, cwd, ts(799), "t4", patch(f"{cwd}/src/middleware.rs", 12,
           ["    let limiter = Limiter::new(60, Duration::from_secs(60));",
            "    chain.push(limiter);"], ["    // TODO: rate limiting"])),
    call(api, cwd, ts(780), "Agent", {"subagent_type": "Explore",
         "description": "find every route that skips middleware"}, "t5"),
    result(api, cwd, ts(700), "t5", {"agentId": "a1f9c22", "status": "done",
           "resolvedModel": "claude-haiku-4-5", "description": "find every route that skips middleware",
           "prompt": "…", "isAsync": False}, text="3 routes bypass the chain: /health, /metrics, /ready"),
    call(api, cwd, ts(660), "Bash", {"command": "cargo test limiter", "description": "run the new tests"}, "t6"),
    result(api, cwd, ts(640), "t6", {"stdout": "", "stderr":
           "error[E0432]: unresolved import `crate::limiter`\n  --> src/middleware.rs:3:5",
           "interrupted": False, "isImage": False}, is_error=True, text="failed"),
    say(api, cwd, ts(630), "The module isn't declared yet — adding it to lib.rs."),
    call(api, cwd, ts(620), "Edit", {"file_path": f"{cwd}/src/lib.rs"}, "t7"),
    result(api, cwd, ts(619), "t7", patch(f"{cwd}/src/lib.rs", 4, ["mod limiter;"], [])),
    call(api, cwd, ts(600), "Bash", {"command": "cargo test limiter", "description": "run the tests again"}, "t8"),
    result(api, cwd, ts(560), "t8", {"stdout": "test limiter::refills_over_time ... ok\ntest limiter::rejects_a_burst ... ok\n\ntest result: ok. 2 passed",
           "stderr": "", "interrupted": False, "isImage": False}),
    {"type": "system", "subtype": "turn_duration", "durationMs": 341000, "messageCount": 24,
     "timestamp": ts(555), "uuid": str(uuid.uuid4()), "isMeta": False, "sessionId": api,
     "cwd": cwd, "version": "2.1.234", "gitBranch": "main", "isSidechain": False,
     "parentUuid": None, "userType": "external", "entrypoint": "cli"},
    prompt(api, cwd, ts(60), "Now add the retry-after header and rerun."),
    call(api, cwd, ts(40), "Bash", {"command": "rm -f target/debug/limiter.tmp"}, "t9"),
]
write("-tmp-ironsight-demo-repo", api, recs)

# ── a session waiting on its human ──────────────────────────────────────────
web = str(uuid.uuid4())
wcwd = "/tmp/ironsight-demo-web"
write("-tmp-ironsight-demo-web", web, [
    {"type": "ai-title", "sessionId": web, "aiTitle": "Dark mode for the dashboard"},
    prompt(web, wcwd, ts(2400), "Give the dashboard a dark theme that follows the system setting."),
    say(web, wcwd, ts(2380), "Done — the palette now switches on prefers-color-scheme."),
    call(web, wcwd, ts(2360), "Edit", {"file_path": f"{wcwd}/styles/theme.css"}, "w1"),
    result(web, wcwd, ts(2359), "w1", patch(f"{wcwd}/styles/theme.css", 1,
           ["@media (prefers-color-scheme: dark) {", "  :root { --bg: #0b1220; }", "}"], [])),
])

# ── a finished session ──────────────────────────────────────────────────────
docs = str(uuid.uuid4())
dcwd = "/tmp/ironsight-demo-docs"
write("-tmp-ironsight-demo-docs", docs, [
    {"type": "ai-title", "sessionId": docs, "aiTitle": "Rewrite the install guide"},
    prompt(docs, dcwd, ts(48000), "Rewrite the install guide for the new packaging."),
    say(docs, dcwd, ts(47900), "Rewritten, with the package manager sections split out."),
])

# ── history, so the conversation browser has something to browse ────────────
# Titles and ages only: the browser reads the head of each transcript, so this
# is all it ever looks at.
HISTORY = [
    ("Fix flaky checkout test", "-tmp-ironsight-demo-repo", "/tmp/ironsight-demo-repo", 5 * 3600),
    ("Port the CLI to clap 4", "-tmp-ironsight-demo-repo", "/tmp/ironsight-demo-repo", 26 * 3600),
    ("Explain the retry budget", "-tmp-ironsight-demo-web", "/tmp/ironsight-demo-web", 3 * 86400),
    ("Draft the release notes", "-tmp-ironsight-demo-docs", "/tmp/ironsight-demo-docs", 4 * 86400),
    ("Trim the docker image", "-tmp-ironsight-demo-repo", "/tmp/ironsight-demo-repo", 6 * 86400),
    ("Audit the auth middleware", "-tmp-ironsight-demo-repo", "/tmp/ironsight-demo-repo", 9 * 86400),
    ("Why is startup 400ms slower", "-tmp-ironsight-demo-repo", "/tmp/ironsight-demo-repo", 12 * 86400),
    ("Set up the staging deploy", "-tmp-ironsight-demo-web", "/tmp/ironsight-demo-web", 18 * 86400),
    ("Write the postmortem", "-tmp-ironsight-demo-docs", "/tmp/ironsight-demo-docs", 25 * 86400),
    ("First look at the codebase", "-tmp-ironsight-demo-repo", "/tmp/ironsight-demo-repo", 40 * 86400),
]
for title, slug, cwd, ago in HISTORY:
    sid = str(uuid.uuid4())
    path = write(slug, sid, [
        {"type": "ai-title", "sessionId": sid, "aiTitle": title},
        prompt(sid, cwd, ts(ago), title.lower() + " please"),
        say(sid, cwd, ts(ago - 30), "Done."),
    ])
    when = time.time() - ago
    os.utime(path, (when, when))

os.makedirs(os.path.join(OUT, "sessions"), exist_ok=True)
print(json.dumps({"dir": OUT, "api_session": api, "web_session": web, "docs_session": docs}))
