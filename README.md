# scope

**Never miss the Claude Code session that is waiting on you.**

![scope](docs/demo.gif)

Run three or four Claude Code sessions at once and the bottleneck is never the
model — it is the session sitting on a permission prompt in a window you are not
looking at. scope watches every session on the machine, puts the blocked one at
the top, and lets you answer it without leaving the window. Then, when you want
the detail, it has everything the transcripts know: every file touched, every
diff, every subagent, every error.

No hooks to install, no server, no configuration, nothing leaves the machine.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/nyfeblade/nyfe-scope/main/install.sh | sh
```

Or, if you would rather not pipe a script into a shell — it is short, read it
first — grab a binary from [releases](https://github.com/nyfeblade/nyfe-scope/releases),
or build it:

```sh
cargo install --git https://github.com/nyfeblade/nyfe-scope
```

Then run `scope`. Linux and macOS. tmux is optional, and only needed to steer
sessions rather than watch them. There is no native Windows build — steering is
built on tmux — but it runs under WSL exactly as it does on Linux, provided the
sessions you want to watch are running inside WSL too.

## The session waiting on you

![approvals](docs/approvals.png)

A session blocked on a prompt is the only state that cannot make progress on its
own, so scope treats it as the most important thing on screen: blocked sessions
sort to the top, the header counts them, and the question with its options
appears at the bottom of the window wherever you are.

- `y` accepts, `d` declines, `ctrl`+digit picks any other option
- `p` jumps to the next session waiting
- `N` turns on desktop notifications, which also fire when a session goes idle
  or hits an error

It works by reading the rendered pane, so it handles any prompt Claude Code
draws — permission requests, trust prompts, plan approvals — without depending
on internals that can change.

From a script, without opening the UI:

```sh
scope waiting              # what is blocked, and what it is asking
scope approve api-7c       # answer it (option 1 by default)
scope adopt nyfe-32        # (re)open a conversation in tmux so it can be steered
scope prune                # close scope sessions whose process has exited
```

## Managing sessions

Press `enter` on a session and scope shows everything you can do to it, with the
reason spelled out for anything it cannot do — so nothing has to be memorised
and no key is a dead end.

![actions](docs/actions.png)

Sessions running in a tmux pane can be driven exactly as you would drive them by
hand, which keeps slash commands, permission prompts and plan mode working. The
same actions have direct keys once you know them:

| key | does |
|---|---|
| `s` | type a message into the selected session |
| `b` | send one message to every steerable session |
| `Q` | queue a message; it is delivered when that session next goes idle |
| | sending to a busy session says so, rather than looking delivered |
| `i` | interrupt (sends Escape) |
| `m` | passthrough — every key goes to the session until `ctrl+]` or `F12` |
| `a` `O` | show it full-screen · open it in its own window |
| `n` `A` | start a new session · adopt a running one, or reopen a stopped one |
| `W` | start one on its own branch in its own checkout |
| `M` `X` | merge that branch back · remove the checkout |
| `K` `P` | stop the session · tidy up finished scope sessions |
| `Z` | stop everything scope started (each one reopens with `A`) |
| `L` | launch a whole fleet from a config file |

When you attach, the session's status line shows the way back, so a
full-screen session is never a one-way door. If scope is itself running inside
tmux it switches the client instead of nesting, and `ctrl+b L` returns.

Sessions started outside tmux are fully visible but cannot be typed into; they
are marked, so it is always clear which is which. `A` moves one: it resumes the
same conversation inside tmux and closes the original window, so the
conversation continues in one place rather than two. It asks first, and it
immediately reopens the session in a fresh window, so nothing you were watching
disappears — `O` does the same for any session on demand.

### Stopping is not losing

`A` is also the way back into a session that has stopped, whether you stopped it,
it crashed, or its terminal closed: the transcript is on disk, so it reopens in
tmux with its history and picks up where it left off. Anything scope can see it
can get back to.

Because of that, nothing is closed on a guess. `P` tidies up only sessions with
no Claude Code process left anywhere in them — a pane sitting at a shell prompt
while a command runs below it is still working, and closing it would throw away a
turn nobody asked to end. When scope cannot tell, it leaves the session alone and
says so.

### Isolated sessions

Several sessions working one repository will trample each other. `W` starts a
session on a fresh branch in its own git worktree, so each edits its own files
and commits its own history while your working tree stays untouched. The tree
pane shows how far ahead the branch is, `M` merges it back with `--no-ff`, `X`
removes the checkout. Worktrees live under
`~/.local/share/nyfe-scope/worktrees/`, well away from the repository.

Merging refuses rather than guesses: if the repository is not on the base branch
it says so and does nothing, and a conflict is reported for you to resolve.

### Fleets

`~/.config/nyfe-scope/fleet.json` describes sessions to start together, and `L`
launches all of them:

```json
[
  {"cwd": "~/api", "prompt": "run the test suite and fix what fails", "effort": "high"},
  {"cwd": "~/web", "model": "claude-opus-5", "permission_mode": "plan"},
  {"cwd": "~/api", "worktree": "refactor-auth", "prompt": "extract the auth middleware"}
]
```

## What it shows

Ten panes, on keys `1` to `9` and `0`. The mouse works too: click a session or
a row to select it, click again to open it, and the wheel scrolls. `--no-mouse`
turns capture off if you would rather keep the terminal's own text selection
(shift-drag usually still selects while capture is on).

| pane | what it is for |
|---|---|
| feed | every event as it happens — prompt, reply, tool call with its arguments, result with status. `enter` opens the whole thing: full command, full output, full diff |
| files | every file read, written or edited, with counts, lines added and removed, and each file's diff history |
| stats | requests by model, tokens, cache, context fill, turns, a tool histogram, tool latency, errors, and a per-minute activity strip |
| plan | the session's todo list as it stands, plus anything queued |
| agents | subagents it launched, with status, duration and their own output |
| mirror | what that session's terminal is showing right now |
| tree | the working directory as it stands: branch, changed files, diffs |
| errors | every failed tool call and API error in one list |
| fleet | every session on a single timeline |
| read | the conversation on its own — what was asked and answered, wrapped and readable, with the machinery left out |

`/` searches everything loaded across all sessions; `]` and `[` step the matches.

![files](docs/files.png)

![stats](docs/stats.png)

Other keys: `enter` on a session opens its actions, `j`/`k` select a session,
`J`/`K` move in the right pane, `f`
filters the feed, `g`/`G` jump to top or bottom, `l` hides sessions with no
running process, `r` rescans, `$` switches between the subscription view and an
API-equivalent cost estimate, `?` shows help, `q` quits — `esc` only dismisses.

## Cost, on a subscription

The default view shows requests and tokens, not dollars, because a subscription
is not billed per token. `$` switches to an estimate of what the same tokens
would have cost at API rates (input, output, cache reads at 0.1x, cache writes
at 1.25x for the five-minute TTL and 2x for the hour). It is a comparison, not a
bill. A `*` means a model with no known rate was seen; the rate table is a
snapshot and will drift.

## What it cannot show

Reasoning text. Claude Code requests thinking with display omitted, so the API
returns empty thinking blocks and empty is what reaches the disk. Across 7,885
thinking blocks on the machine this was built on, two had text, both from a
local non-Claude model. No transcript reader can show what was never written.
Everything else a session does is there in full.

## How it works

Two files Claude Code already writes.

- `~/.claude/projects/<project>/<session-id>.jsonl` — the transcript, appended
  line by line *while a turn is in flight*. Tailing it gives every prompt, tool
  call, result, token count, turn duration and error as they happen.
- `~/.claude/sessions/<pid>.json` — the live-session registry: pid, cwd, the
  name Claude Code derived, and a busy/idle status. A pid is believed only when
  its start time matches the one recorded, so a recycled pid never reads as a
  live session. Versions that do not write this file fall back to inferring
  activity from transcript recency.

`CLAUDE_CONFIG_DIR` is honoured; `--root` points at transcripts anywhere.

Both formats are Claude Code's own and undocumented, so they can change. scope
parses defensively and says so in the footer when it meets a version it was not
built against or a transcript it cannot read, rather than quietly showing
figures that are wrong. It was built against Claude Code 2.1.x.

Read that as the maintenance promise it is: a Claude Code release can move a
field, rename a status or redraw a prompt, and when that happens scope loses a
detail — a status that reads wrong, a prompt it no longer recognises — rather
than inventing one. If you meet that, open an issue with your Claude Code
version; the fix is usually a few lines. The same is true of the pane reading
behind approvals and passthrough, which works by looking at what a session has
drawn on screen.

## Development

```sh
cargo build --release
cargo test                  # prompt parser and git worktree lifecycle
scripts/demo.sh             # a synthetic ~/.claude plus a session parked on a
                            # prompt, for screenshots and manual testing
```

`scripts/demo.sh` prints how to run against the fixture and how to tear it down.
Nothing in it touches your real sessions.

## License

MIT
