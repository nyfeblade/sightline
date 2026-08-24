#!/usr/bin/env bash
# Watch `ironsight glue` reconcile a fork that has genuinely diverged.
#
#   ./run.sh            build the scenario and show what a plain merge does
#   ./run.sh --brief    also print the packet the reconciling agent is given
#   ./run.sh --start    also start the reconciliation (this runs a real agent)
#
# Everything happens in /tmp/glue-demo and a scratch Ironsight state directory.
# Your own fleet, your own session list and your own settings are not touched.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEMO=/tmp/glue-demo
export IRONSIGHT_DATA_DIR=/tmp/glue-demo-state

IRONSIGHT="${IRONSIGHT:-$(command -v ironsight || true)}"
if [ -z "$IRONSIGHT" ]; then
  echo "ironsight is not on PATH. Set IRONSIGHT=/path/to/ironsight and try again." >&2
  exit 1
fi

say()  { printf '\n\033[1;36m── %s\033[0m\n' "$*"; }
note() { printf '   %s\n' "$*"; }
# Pauses so you can read, when there is somebody there to read it.
pause() {
  [ -t 0 ] || return 0
  printf '\n\033[2m   [enter]\033[0m'
  read -r _ || true
}

rm -rf "$IRONSIGHT_DATA_DIR"; mkdir -p "$IRONSIGHT_DATA_DIR"

say "1. A fork that has drifted"
bash "$HERE/setup.sh" "$DEMO" >/dev/null
cd "$DEMO"
git log --oneline --all --graph
note ""
note "upstream renamed send() to emit() and changed what it returns."
note "the fork patched that same file with a mute switch, and built three"
note "functions on the old name — every one of which tags its message."
note ""
note "the fork is green today:"
python3 -m pytest -q 2>&1 | tail -1
pause

say "2. What a plain git merge does with this"
git checkout -qb naive 2>/dev/null
git merge v2.0.0 2>&1 | tail -2 || true
note ""
note "so you resolve it the way anyone would, by taking upstream's file:"
git checkout --theirs notify.py >/dev/null 2>&1 || true
git add notify.py >/dev/null 2>&1 || true
git commit -qm "merge, taking upstream" >/dev/null 2>&1 || true
python3 -m pytest -q 2>&1 | tail -2 || true
note ""
note "the build is broken, which you would notice. The mute switch is also"
note "gone, which you would not — no test upstream wrote covers it, and the"
note "fork's own test for it went out with the conflict. That is the failure"
note "glue is pointed at: not the conflict, the thing lost while resolving it."
git checkout -q main; git branch -qD naive; rm -rf __pycache__ .pytest_cache
pause

say "3. What this fork says must never stop being true"
sed -n '/\[\[invariant\]\]/,$p' .ironsight/checks.toml | grep -E '^(name|must)' | sed 's/^/   /'
note ""
note "these are commands that must FAIL. One that succeeds has found the very"
note "thing it was written to look for. Approving them first, because nothing"
note "runs out of a repository until it has been read:"
"$IRONSIGHT" trust "$DEMO" | tail -3
note ""
"$IRONSIGHT" invariants
pause

say "4. What glue sees"
"$IRONSIGHT" glue v2.0.0 --dry-run 2>&1 | head -22
if [ "${1:-}" = "--brief" ]; then
  note ""
  note "the whole packet the reconciling agent is opened with:"
  "$IRONSIGHT" glue v2.0.0 --dry-run 2>&1 | tail -n +2
fi
pause

if [ "${1:-}" != "--start" ]; then
  say "To run it"
  note "$0 --start"
  note ""
  note "That starts a real agent, in a worktree of its own, and costs real"
  note "tokens. It will not touch this fork's branch and will not merge."
  exit 0
fi

say "5. Reconciling"
note "a worktree is cut, the ability is installed into it, and an agent is"
note "briefed and started there. Watch it in another terminal with:"
note ""
note "   IRONSIGHT_DATA_DIR=$IRONSIGHT_DATA_DIR ironsight"
note ""
"$IRONSIGHT" glue v2.0.0

WORKTREE="$("$IRONSIGHT" owned | awk '{print $NF}' | head -1)"
say "6. Watching"
note "worktree: $WORKTREE"
note ""
note "while it works:   IRONSIGHT_DATA_DIR=$IRONSIGHT_DATA_DIR ironsight owned"
note "when it is idle:  the reconciliation is done, and then you judge it:"
note ""
note "   cd $WORKTREE"
note "   python3 -m pytest -q"
note "   IRONSIGHT_DATA_DIR=$IRONSIGHT_DATA_DIR ironsight invariants"
note ""
note "the checks say whether it built. The invariants say whether it kept the"
note "things the fork could not afford to lose. Nothing merged: the branch is"
note "there for you to take or throw away."
