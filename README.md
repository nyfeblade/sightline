# scope

Live view of every Claude Code session on your machine — what each one is doing
right now, which files it is touching, what it changed, what it is spending —
and, where sessions run in tmux, a way to steer them from the same window.

No hooks to install, no server, no configuration. Nothing leaves the machine.

```
scope                    # live view of sessions touched in the last 24h
scope --live             # only sessions with a running claude process
scope --once             # one-shot table, for scripts
scope new [path]         # start a session in tmux (so it can be steered)
scope send <who> <text>  # type a line into a running session and submit it
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

**feed** — the running commentary. One line per event: prompt, reply, tool call
with its arguments, result with size and status. `enter` opens the whole thing —
the full command, the full output, the full diff.

**files** — every file the session read, wrote or edited, with per-file counts,
lines added and removed, and when it was last touched. `enter` shows that file's
diff history for the session, reconstructed from the patches Claude Code records.

**stats** — requests by model, tokens in and out, cache reads and writes, how
full the context window is, turns with average and longest, a tool-call
histogram, tool latency average and worst, files and line churn, error count,
and a per-minute activity strip for the last hour.

## Steering sessions

Claude Code's own cross-session channel is a token-authenticated private socket.
scope does not touch it. Instead it types into the terminal, which means a
session running in a tmux pane can be driven exactly as a person would drive it —
permission prompts, slash commands, plan mode and everything else keep working.

Start sessions with `scope new`, or `n` inside scope, and they become steerable.
Sessions started outside tmux are fully visible but cannot be typed into; they
are marked in the list, so it is always clear which is which. Everything here is
a no-op with a clear message when tmux is not installed.

| key | does |
|---|---|
| `s` | type a message into the selected session |
| `b` | send one message to every steerable session |
| `i` | interrupt the selected session (sends Escape) |
| `a` | attach to it full-screen; detach and you are back in scope |
| `n` | start a new session in tmux |

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
