#!/usr/bin/env bash
# Stand up a self-contained demo: synthetic transcripts, a git repo with real
# changes, and one tmux session parked on a permission prompt so the approvals
# path is exercised for real rather than mocked.
#
# usage: scripts/demo.sh [fixture-dir]    then: CLAUDE_CONFIG_DIR=<dir> Ironsight
set -euo pipefail

FIXTURE="${1:-/tmp/ironsight-fixture}"
REPO=/tmp/ironsight-demo-repo
HERE="$(cd "$(dirname "$0")" && pwd)"

rm -rf "$FIXTURE" "$REPO"
INFO="$(python3 "$HERE/demo-fixture.py" "$FIXTURE" "$REPO")"
API_SESSION="$(printf '%s' "$INFO" | python3 -c 'import json,sys; print(json.load(sys.stdin)["api_session"])')"

# A repository with committed history, a modified file and an untracked one, so
# the tree pane has something honest to show.
mkdir -p "$REPO/src"
cd "$REPO"
git init -q -b main
printf 'pub mod middleware;\nmod limiter;\n' > src/lib.rs
printf 'pub struct Chain;\n' > src/middleware.rs
git add . && git -c user.email=demo@example.com -c user.name=demo commit -qm "initial"
printf 'pub struct Chain;\n\nimpl Chain {\n    pub fn push(&mut self, _l: Limiter) {}\n}\n' > src/middleware.rs
printf 'pub struct Limiter {\n    capacity: u32,\n}\n' > src/limiter.rs

# A session parked on a permission prompt, drawn the way Claude Code draws one.
tmux kill-session -t demo-agent 2>/dev/null || true
tmux new-session -d -s demo-agent -x 100 -y 30 -c "$REPO" \
  'printf "\n  Bash command\n    rm -rf target/debug\n    Remove the stale build output\n\n  This command requires approval\n  Do you want to proceed?\n  \xe2\x9d\xaf 1. Yes\n    2. Yes, and don'"'"'t ask again for rm commands in this project\n    3. No, and tell Claude what to do differently\n\n  Esc to cancel \xc2\xb7 Tab to amend\n"; sleep 3600'
sleep 0.5

PANE_PID="$(tmux list-panes -t demo-agent -F '#{pane_pid}')"
# field 22 of /proc/<pid>/stat, counted after the comm field, is the start time
PROC_START="$(sed -e 's/.*) //' /proc/"$PANE_PID"/stat | cut -d' ' -f20)"

mkdir -p "$FIXTURE/sessions"
cat > "$FIXTURE/sessions/$PANE_PID.json" <<JSON
{"pid": $PANE_PID, "sessionId": "$API_SESSION", "cwd": "$REPO",
 "procStart": "$PROC_START", "version": "2.1.234", "kind": "interactive",
 "entrypoint": "cli", "name": "api-7c", "nameSource": "derived",
 "status": "busy", "updatedAt": 0, "statusUpdatedAt": 0}
JSON

echo "fixture:  $FIXTURE"
echo "repo:     $REPO"
echo "run:      CLAUDE_CONFIG_DIR=$FIXTURE Ironsight --since 30d"
echo "teardown: tmux kill-session -t demo-agent; rm -rf $FIXTURE $REPO"
