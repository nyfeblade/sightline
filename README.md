# nyfe scope

Live view of every Claude Code session on this machine — what it is doing right
now, which files it is touching, what it changed, and what it is spending.

```
scope                 # live view, sessions touched in the last 24h
scope --live          # only sessions with a running claude process
scope --once          # one-shot table, for scripts
scope --view files    # start on the files pane
scope --cost          # show API-equivalent cost instead of the subscription view
```

## Where the data comes from

Two local sources, both written by Claude Code itself. No hooks to install, no
server to run, and nothing is sent anywhere.

- `~/.claude/projects/<project>/<session-id>.jsonl` — the transcript. Claude Code
  appends to it line by line *while a turn is in flight*, so tailing it is a live
  feed: every prompt, every tool call with its full input, every result, token
  usage per request, turn durations, errors.
- `~/.claude/sessions/<pid>.json` — the live-session registry: pid, cwd, derived
  name, and a `busy`/`idle` status. A pid is only trusted when its start time
  matches the registry's, so a recycled pid never reads as a live session.

Totals are computed over the whole transcript; the feed keeps the last 4000
events per session.

## Panes

- **feed** — the running commentary. One line per event: prompt, reply, tool call
  with its arguments, result with size and status. `enter` opens the full text —
  the entire command, the entire output, the entire diff.
- **files** — every file the session read, wrote, or edited, with per-file counts,
  lines added and removed, and when it was last touched. `enter` shows that
  file's diff history for the session.
- **stats** — requests by model, tokens in/out, cache read/write, how full the
  context window is, turns with average and longest, tool call counts and
  latencies, files and line churn, errors, and a per-minute activity strip.

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
| `q` | quit |

## Cost, on a subscription

The default view shows requests and tokens, not dollars — a Claude subscription
is not billed per token. `$` switches to an estimate of what the same tokens
would have cost at first-party API rates (input, output, cache reads at 0.1x,
cache writes at 1.25x for the 5-minute TTL and 2x for the hour). It is a
comparison, not a bill. A `*` means a model with no known rate was seen.

## What it cannot show

Reasoning text. Claude Code requests thinking with display omitted, so the API
returns empty thinking blocks and empty is what lands on disk — across 7,885
thinking blocks in this machine's transcripts, exactly 2 carried text, both from
a local non-Claude model. No transcript reader can show what was never written.
Everything else the session did is there in full.

## Build

```
cargo build --release
```

Rust, ratatui, and nothing else at runtime.
