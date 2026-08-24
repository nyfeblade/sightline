#!/usr/bin/env bash
# Build a fork that has genuinely diverged from its upstream, so `sightline glue`
# has something real to reconcile.
#
# The scenario is small enough to read in a minute and awkward enough to be
# worth watching:
#
#   upstream  renamed its only function and changed what it returns
#   the fork   patched that same file, and built a feature on the old name
#
# A plain `git merge` here either stops with conflict markers or, if you take
# one side, quietly drops something. Which side you lose depends on which you
# picked, and nothing tells you.
set -euo pipefail

WHERE="${1:-/tmp/glue-demo}"
rm -rf "$WHERE"
mkdir -p "$WHERE"
cd "$WHERE"

git init -qb main
git config user.email demo@example.com
git config user.name "Glue Demo"

# ── the common ancestor: upstream v1 ─────────────────────────────────────────
cat > notify.py <<'EOF'
"""Upstream's notifier, version 1."""


def send(message):
    """Return the line that would be sent."""
    return f"notify: {message}"
EOF
git add -A && git commit -qm "upstream v1.0.0: notify.send()"
git tag v1.0.0

# ── upstream moves on, and breaks its own interface ──────────────────────────
git checkout -qb upstream-line
cat > notify.py <<'EOF'
"""Upstream's notifier, version 2.

`send` is gone. Everything goes through `emit`, which takes a level and puts it
at the front of the line.
"""

LEVELS = ("info", "warn", "error")


def emit(message, level="info"):
    """Return the line that would be sent, at a level."""
    if level not in LEVELS:
        raise ValueError(f"unknown level: {level}")
    return f"{level}: {message}"
EOF
cat > test_notify.py <<'EOF'
from notify import emit


def test_default_level_is_info():
    assert emit("hello") == "info: hello"


def test_a_level_can_be_chosen():
    assert emit("careful", "warn") == "warn: careful"


def test_an_unknown_level_is_refused():
    try:
        emit("hello", "shout")
    except ValueError:
        return
    raise AssertionError("an unknown level should be refused")
EOF
git add -A && git commit -qm "upstream v2.0.0: send() becomes emit(), with levels"
git tag v2.0.0

# ── meanwhile, the fork ──────────────────────────────────────────────────────
git checkout -q main

# The fork patched upstream's file. This is the contested one.
cat > notify.py <<'EOF'
"""Upstream's notifier, version 1 — with the fork's mute switch."""

MUTED = False


def send(message):
    """Return the line that would be sent, unless we are muted."""
    if MUTED:
        return ""
    return f"notify: {message}"
EOF

# And built its own feature on top of the old name.
cat > app.py <<'EOF'
"""The fork's own code. Every message it sends is tagged with the estate."""

from notify import send

PREFIX = "[acme] "


def alert(text):
    return send(PREFIX + text)


def page(who, text):
    return send(PREFIX + f"{who}: {text}")


def audit(action):
    return send(PREFIX + f"audit {action}")
EOF

cat > test_app.py <<'EOF'
import notify
from app import alert, audit, page


def test_every_message_is_tagged():
    assert "[acme]" in alert("disk full")
    assert "[acme]" in page("sam", "disk full")
    assert "[acme]" in audit("login")


def test_muting_silences_everything():
    notify.MUTED = True
    try:
        assert alert("disk full") == ""
    finally:
        notify.MUTED = False
EOF

mkdir -p .sightline
cat > .sightline/checks.toml <<'EOF'
# What finished means in this fork.

[[check]]
name    = "tests"
run     = "python3 -m pytest -q"
timeout = "2m"

# And what must never stop being true, whatever upstream does.
#
# This one is the point of the demo. Every message this fork sends is tagged
# with the estate it came from, and an upstream merge that quietly dropped the
# tag would still compile and would still pass a suite somebody had "fixed".
# The command below succeeds only when the tag is gone — so it firing is the
# bad news.
[[invariant]]
name   = "every message carries the estate tag"
must   = "app.py sends nothing without PREFIX. Losing the tag is how an alert stops being attributable, and no test upstream writes will ever notice."
refute = "grep -q 'def alert' app.py && ! grep -q 'PREFIX' app.py"

[[invariant]]
name   = "the mute switch survives"
must   = "notify.py keeps a way to silence everything. It is the fork's own patch to upstream's file and the merge is where it gets lost."
refute = "! grep -q 'MUTED' notify.py"
EOF

cat > .sightline/constitution.md <<'EOF'
# Constitution

## Mission
A notifier for the acme estate. Every message is attributable to it.

## Architecture
notify.py is upstream's and is patched as little as possible. app.py is ours.

## Constraints
- Every message this fork sends carries the estate tag.
- The mute switch must keep working; it is what we use during maintenance.
- Do not edit tests to make a merge pass.

## Rejected
- Vendoring upstream and never updating. That is where we were, and it is why
  this exists.

## Done means
- python3 -m pytest -q passes, and `sightline invariants` is quiet.
EOF

cat > README.md <<'EOF'
# The acme notifier

A fork of upstream's `notify`, with a mute switch and an estate tag.
EOF

git add -A
git commit -qm "the fork: a mute switch, and every message tagged"

# Upstream is a remote as far as the fork is concerned. It is this same
# repository, because the demo has to work with no network.
git remote add upstream "$WHERE"
git fetch -q upstream 2>/dev/null || true

echo "built the fork at $WHERE"
