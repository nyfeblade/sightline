# scope

Live view of every Claude Code session on your machine — what each one is doing
right now, which files it is touching, what it changed, what it is spending —
and, where sessions run in tmux, a way to steer them from the same window.

No hooks to install, no server, no configuration. Nothing leaves the machine.

```
scope                     # live view of sessions touched in the last 24h
scope --live              # only sessions with a running claude process
scope --once              # one-shot table, for scripts
scope new [path]          # start a session in tmux (so it can be steered)
scope send <who> <text>   # type a line into a running session and submit it
scope waiting             # list sessions blocked on a prompt
scope approve <who> [n]   # answer a blocked session, default option 1
scope new <path> --worktree <branch>   # …on its own branch, in its own checkout
```

## Where the data comes from

Two files Claude Code already writes.

- `~/.claude/projects/<project>/<session-id>.jsonl` — the transcript, appended
  line by line *while a turn is in flight*. Tailing it gives every prompt, every
  tool call with its full input, every result, per-request token usage, turn
  durations and errors, as they happen.
- `~/.claude/sessions/<pid>.json` — the live-session registry: pid, cwd, the
  name Claude Code derived for the session, and a busy/idle status. A pid is
  only believed when its start time matches the one recorded, so a recycled pid
  never reads as a live session. Older Claude Code versions don't write this
  file; scope then infers activity from transcript recency instead.

`CLAUDE_CONFIG_DIR` is honoured, and `--root` points at transcripts anywhere.

## Panes

Nine, on keys `1` to `9`.

**feed** — the running commentary. One line per event: prompt, reply, tool call
with its arguments, result with size and status. `enter` opens the whole thing —
the full command, the full output, the full diff. Where Claude Code spilled a
large result to a file, `enter` reads that file, so you see what was actually
produced rather than the preview.

**files** — every file the session read, wrote or edited, with per-file counts,
lines added and removed, and when it was last touched. `enter` shows that file's
diff history for the session, reconstructed from the patches Claude Code records.

**stats** — requests by model, tokens in and out, cache reads and writes, how
full the context window is, turns with average and longest, a tool-call
histogram, tool latency average and worst, files and line churn, error count,
and a per-minute activity strip for the last hour.

**plan** — the session's todo list as it stands, plus prompts it has queued and
any message scope is holding for it.

**agents** — subagents the session launched, with kind, description, status and
duration. `enter` reads the subagent's own output file.

**mirror** — what the session's terminal is showing right now, read from its
tmux pane. Press `m` to type into it directly.

**tree** — the working directory as it stands: branch, changed files, unstaged
insertions and deletions. `enter` shows the diff for one file. This is what
landed on disk, which is not always what the transcript claims.

**errors** — every failed tool call and API error in the session, in one list.

**fleet** — every session on a single timeline, tagged by session.

`/` searches everything loaded, across all sessions; `]` and `[` step through
the matches.

## Steering sessions

Claude Code's own cross-session channel is a token-authenticated private socket.
scope does not touch it. Instead it types into the terminal, which means a
session running in a tmux pane can be driven exactly as a person would drive it —
permission prompts, slash commands, plan mode and everything else keep working.

Start sessions with `scope new`, or `n` inside scope, and they become steerable.
A session already running outside tmux can be adopted with `A`, which resumes
that same conversation inside tmux — you then close the old window. Sessions
that are not steerable are fully visible and marked as such, so it is always
clear which is which. Everything here is a no-op with a clear message when tmux
is not installed.

| key | does |
|---|---|
| `s` | type a message into the selected session |
| `b` | send one message to every steerable session |
| `Q` | queue a message; scope sends it when that session next goes idle |
| `y` `d` | accept or decline what a session is asking |
| `ctrl`+digit | pick another option on that prompt |
| `p` | jump to the next session waiting on you |
| `i` | interrupt the selected session (sends Escape) |
| `m` | passthrough: every key goes to the session until `ctrl+]` |
| `a` | attach to it full-screen; detach and you are back in scope |
| `n` `A` | start a new session · adopt an existing one into tmux |
| `W` | new session on its own branch and checkout |
| `M` `X` | merge that branch back · remove the checkout |
| `L` | launch a fleet from `~/.config/nyfe-scope/fleet.json` |
| `N` | desktop notifications on or off |

### Waiting on you

A session blocked on a permission prompt cannot make progress until a person
answers, so scope treats that as the most important state there is: blocked
sessions sort to the top, the header counts them, and the prompt — question and
options — appears at the bottom of the window wherever you are, answerable with
one key. Detection reads the rendered pane, so it works for any prompt Claude
Code draws without depending on its internals.

### Notifications and queues

`N` toggles desktop notifications, which fire when a session goes idle, hits an
error, or starts waiting on you. `Q` queues a message for a session that is
mid-turn; scope delivers it the moment that session is free, so you can line up
the next instruction without watching for the gap.

### Isolated sessions

Several sessions working the same repository will trample each other. `W` starts
one on a fresh branch in its own checkout — a git worktree — so each session
edits its own files and commits its own history while the original working tree
stays untouched. The tree pane then shows how many commits that branch is ahead
of the base, `M` merges it back with `--no-ff`, and `X` removes the checkout.
Worktrees live under `~/.local/share/nyfe-scope/worktrees/<repo>/<branch>`, well
away from the repository, so they never show up as untracked noise.

Merging refuses rather than guesses: if the repository is not on the base branch
it says so and does nothing, and a conflicting merge reports the conflict and
leaves the tree for you to resolve.

### Fleets

`~/.config/nyfe-scope/fleet.json` is a list of sessions to start together:

```json
[
  {"cwd": "~/api", "prompt": "run the test suite and fix what fails", "effort": "high"},
  {"cwd": "~/web", "model": "claude-opus-5", "permission_mode": "plan"},
  {"cwd": "~/api", "worktree": "refactor-auth", "prompt": "extract the auth middleware"}
]
```

`L` launches all of them.

`scope send` accepts a session's Claude Code name, its id, or the tmux session
name — the last of which works even before a session has written a transcript,
such as while it is still asking whether it trusts the folder.

## Keys

| key | does |
|---|---|
| `j` `k` | select session, or move in the focused pane |
| `J` `K` | move in the right pane from anywhere |
| `1` `2` `3` | feed · files · stats |
| `enter` `v` | open the full text of the selected item |
| `f` | filter the feed: all, tools, bash, files, talk |
| `g` `G` | top / bottom (`G` resumes following) |
| `$` | subscription view or API-equivalent cost |
| `l` | only sessions with a running process |
| `tab` | switch pane focus |
| `r` | rescan for new sessions |
| `?` | help |
| `q` | quit |

## Cost, on a subscription

The default view shows requests and tokens, not dollars — a Claude subscription
is not billed per token. `$` switches to an estimate of what the same tokens
would cost at first-party API rates (input, output, cache reads at 0.1x, cache
writes at 1.25x for the five-minute TTL and 2x for the hour). It is a
comparison, not a bill, and it is labelled as such. A `*` means a model with no
known rate was seen; the rate table is a snapshot and will drift.

## What it cannot show

Reasoning text. Claude Code requests thinking with display omitted, so the API
returns empty thinking blocks, and empty is what reaches the disk. Across 7,885
thinking blocks on the machine this was built on, two had text — both from a
local non-Claude model. No transcript reader can show what was never written.
Everything else a session does is there in full.

## Install

```
cargo build --release
cp target/release/scope ~/.local/bin/
```

Linux and macOS. Rust, ratatui, and nothing else at runtime. tmux is optional
and only needed for steering. Very large transcripts replay their last 32 MB at
startup so it opens instantly regardless of history size; `NO_COLOR` and
`--plain` drop the palette for terminals that want no colour.

## License

MIT
