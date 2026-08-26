# Sightline

**A control plane for coding agents: it decides what they may do, at the moment
they ask.**

![the app](docs/app.png)

Coding agents are trusted or they are watched, and neither scales. Trusting one
means finding out afterwards. Watching one means you are the bottleneck, and you
are not watching the other three.

Sightline is the third option. It hosts Claude Code rather than sitting beside
it, so every tool call an agent makes stops at a boundary Sightline owns — in
Rust, before the call happens — where it is allowed, refused, amended, or handed
to you. Most of them never reach you. That ratio is the point.

Nothing leaves the machine. No hooks, no server, no API key: it drives the
`claude` binary you already have, on the subscription you already pay for.

## What that gets you

The boundary is only a mechanism. What makes it worth having is that Sightline
knows things no single session can:

| | |
| --- | --- |
| **ceiling** | what the whole fleet has spent, and how many are running. A session cannot answer this in its own favour — the file lives outside every worktree. |
| **scope** | a worker writes inside the directory it owns. A stray absolute path into the main checkout is *redirected* there rather than refused, because it is nearly always a stale path and not an intention. |
| **task** | Sightline wrote down what this session was assigned, and the session has never seen that record. A write with no assignment is work nobody will check. |
| **trust** | a project's `checks.toml` is shell that arrived with someone else's code. It does not run until a person approved those exact commands — including when an agent reads the file and runs what it found. |
| **collision** | two live sessions about to change one file. Nothing else on the machine can answer this, because answering it means seeing the other session. Both would have reported success. |
| **forbid** | `git push --force` and its friends, whatever the permission mode says. |

Four answers are possible at that boundary — allow, deny, rewrite, escalate —
and `rewrite` is the one with no equivalent in a settings file or a hook. A gate
that can only say yes or no has to escalate every ambiguous call to a person.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/nyfeblade/sightline/master/install.sh | sh
```

Or grab a binary from [releases](https://github.com/nyfeblade/sightline/releases),
or build it — the install script is short, and worth reading before you pipe it
into a shell:

```sh
cargo install --git https://github.com/nyfeblade/sightline
```

As a desktop app on Linux, one file, nothing installed:

```sh
scripts/appimage.sh        # builds dist/sightline-<version>-x86_64.AppImage
```

Clicking it opens the window; any argument goes to the commands, so
`./sightline-0.4.1-x86_64.AppImage doctor` works from the same file.
`scripts/desktop-entry.sh` puts it in your application menu instead. The app
needs webkit2gtk, which a normal desktop already has.

On Windows the app is an MSI — WiX 3, because that is what Tauri 2 actually
builds, not an NSIS stub labelled as one. Grab `Sightline_*_x64_en-US.msi`
from [releases](https://github.com/nyfeblade/sightline/releases) once a tagged
build has produced it, or from the `sightline-windows-msi` CI artifact. It is
not in the CLI zip, and `sightline-gui` is not a command that zip ever
contained (v0.4.1 shipped `ironsight-v0.4.1-x86_64-pc-windows-msvc.zip`, CLI
only).

To build the MSI yourself, on Windows, with [WiX Toolset v3](https://wixtoolset.org/)
on PATH (`light.exe` — WiX 4 will not do):

```powershell
cd crates/gui
cargo tauri build --features custom-protocol --bundles msi
```

The commands are a separate zip. `install.ps1` finds whatever prefix the
latest release actually used (`sightline`, `ironsight`, or `scope`) and
installs `sightline.exe`:

```powershell
irm https://raw.githubusercontent.com/nyfeblade/sightline/master/install.ps1 | iex
```

Linux, macOS and Windows; tmux is optional on the first two, and only for
steering sessions rather than watching them. Windows has never been run on
Windows: it compiles, CI builds the MSI, and the pty backend is unit-tested
— which is not the same thing.

## Two kinds of session, and the difference matters

**Watched** — the ones you start yourself, in a terminal. Sightline reads their
transcripts from outside: what they are running, what they touched, what they
are blocked on. It is not in the loop, so none of the kernels above apply. This
is what Sightline did before, and it still does it.

**Owned** — the ones Sightline holds. Claude Code over pipes; Cursor CLI as a
chat plus `--resume`; Grok Bot as a connected Cursor desktop assistant, not a
CLI, whose messages wait in a mailbox a later turn reads with `inbox`. It
answers permission requests at whatever door that vendor actually has, and
serves them the same kernel tools. Everything on this page about deciding
applies here and only here.

Nothing about the first kind changed. If you want an agent governed, Sightline
has to be the one holding it.

## Supervision, without a story about supervision

A chief is a session with no way to start a process. What it has instead is a
tool that asks:

```
assign(path, task)   start a worker on one assignment
fleet()              every worker, whether it is busy, what it is doing
tell(who, text)      say something to a worker you started
claim(summary)       say your work is finished, and find out what the checks said
inbox()              pending tell/assignment, for a worker that is not holding a pipe
```

That is not a formality. A worker Sightline starts is confined to its directory,
counted against the ceilings, policed on every call, and stopped when the fleet
is stopped. One a chief started for itself would be none of those things — so
there is no way to start one, and nothing to route around.

A worker cannot start workers. The tree stays one deep until somebody decides
otherwise, deliberately.

## What "done" means here

A worker saying it has finished is worth nothing on its own.

```
Claimed    the agent says so.
Checked    the suite passed — which says the failures it can express did not
           happen, and nothing more.
Verified   something written to show the work wrong was run, did not fire, and
           has been seen to fire at some point.
```

That last clause is the one people drop, and dropping it is how a suite of
refutations that could never fire verifies everything forever. `sightline refute
<task> <command>` adds one; a refutation that has never caught anything is
reported as exactly that.

Alongside the checks are **invariants** — commands that must *fail*, each written
to succeed only when a guarantee has stopped being true. A passing suite survives
a change that quietly broke something load-bearing; a command looking for the
breakage does not. This repository holds ten of its own, and every one was
checked by breaking the thing on purpose and watching it catch it.

```sh
sightline trust .          # read a project's checks, then approve those commands
sightline check <who>      # what they say about a session's work
sightline invariants       # try to break what must never stop being true
```

## Ceilings

Something that starts sessions on your behalf does not start without a number it
cannot change.

```sh
sightline limits --sessions 4 --spend 20 --window 24h
```

Kept in Sightline's own directory, outside every worktree. A project may lower
them and never raise them, and `sightline chief` refuses to start without them.

## What is proved, and how to check

The design rests on Claude Code's control protocol, which is undocumented. So it
is not assumed — it is established by experiment, and the experiment is in the
repository:

```sh
python3 docs/probes/control_protocol.py all
```

Six properties, each of which can actually be wrong: that every tool call reaches
the host, that a refusal *stops* it, that the host can rewrite a call before it
runs, that `initialize` reports the models and effort levels available, that a
turn can be interrupted mid-tool, and that the model will call a tool the host
serves in-process. Last run against Claude Code 2.1.241 on a Claude Max
subscription: 6/6.

Two live end-to-end runs are examples rather than tests, because they spend
quota:

```sh
cargo run -p sightline-core --example gate_live     # the boundary holds
cargo run -p sightline-core --example chief_live    # chief → worker → real task
```

## Everything else it still does

The monitoring half did not go anywhere, and it is what you will look at most.

- **The session waiting on you**, at the top, answerable without leaving the
  window — including the prompt Claude Code has drawn but not yet written down.
- **The feed**: every prompt, tool call, result, diff, subagent and error, live,
  while a turn is in flight.
- **Files, git tree, plan, stats, errors** per session, and a reading view for
  any file a session touched.
- **Resuming anything, ever** — every conversation on the machine, however old,
  reopened where it stopped.
- **Cost**, as the subscription view or the API-equivalent estimate.
- **Notifications** when something needs you, and a machine-readable stream at
  `sightline events --json` for anything else that wants to watch.
- **Isolated sessions** on their own branch and checkout, merged back when you
  say so.
- **Other agents**: Cursor (`cursor-agent`) and Grok Bot (the Cursor desktop
  assistant, `agent = "grok"`) are first-class vendors next to Claude Code.
  Aider has a real adapter; Codex and Gemini are read from the screen.
- **Glue** — `sightline glue <version>` reconciles a fork onto a newer upstream
  release by teaching your own agent upstream's architecture, seams and
  invariants, then having it write the adapters in a worktree of its own.

## How it works

Two files Claude Code already writes, plus one protocol it speaks.

- `~/.claude/projects/<project>/<session-id>.jsonl` — the transcript, appended
  while a turn is in flight. Tailing it gives every prompt, tool call, result,
  token count and error as they happen.
- `~/.claude/sessions/<pid>.json` — the live-session registry. A pid is believed
  only when its start time matches the one recorded, so a recycled pid never
  reads as a live session.
- **The control protocol**, for sessions Sightline owns: stream-json over two
  pipes, with an MCP server Sightline serves in-process. This is where permission
  decisions arrive and where the kernel's own tools live. No SDK, no Node, no
  API key.

`CLAUDE_CONFIG_DIR` is honoured; `--root` points at transcripts anywhere.

Steering a watched session needs the terminal it is in, and that is the one part
with two implementations: tmux on Unix, a pseudo-console Sightline owns on
Windows. Both answer the same two questions — what is on this screen, and take
this key — so everything above them is written once.

Both file formats are Claude Code's own and undocumented, so they can change.
Sightline parses defensively and says so when it meets a version it was not built
against, rather than quietly showing figures that are wrong. Read that as the
maintenance promise it is: a release can move a field or redraw a prompt, and
when it does Sightline loses a detail rather than inventing one. Open an issue
with your Claude Code version; the fix is usually a few lines.

## Architecture

`crates/core` holds everything that is not a way of looking at it; `crates/tui`
is the commands; `crates/gui` is the window. Neither front end may grow logic the
other needs — sending a message, answering a prompt, reaching a verdict on
whether work is done: one implementation, two callers. There are invariants that
fire if that stops being true.

`docs/ARCHITECTURE.md` is the current design and the evidence under it.
`docs/PLATFORM.md` and `docs/BUILD.md` are the layers and the working spec for
each. `docs/STATE.md` is the shorter, more perishable one: what works today,
what is built but not wired, and what would trip someone up if nobody said so.

Everything claimed to exist is checkable in this repository; everything else is
marked as a claim.

## Honest gaps

- The **single prompt** — one place to say what you want and be asked about the
  rest — is designed and not built. Today that is the window and the commands.
- **Windows has never been run on Windows**, and the macOS app has never been run
  on macOS. Both compile and cross-check clean, and CI builds a WiX 3 MSI, which
  is not the same thing as a session started there.
- **Escalation** currently means the kernels abstain and allow. Holding a call
  open while a person decides is the next piece.
- The chief has run **well, and briefly**. Evidence that it starts and orients;
  no evidence about a long run.
- **Quota is the binding constraint**, not the design. A live chief has already
  been stopped by a rate limit.

## Development

```sh
cargo build --release
cargo test
sightline invariants        # what must never stop being true here
scripts/demo.sh             # a synthetic ~/.claude, for screenshots and manual
                            # testing — nothing in it touches your real sessions
```

Sightline was called Ironsight, and before that scope. Old `.ironsight/` project
directories and `IRONSIGHT_DATA_DIR` are still read: a project's constitution and
checks are committed with its code, and there is no upgrade step anyone would
think to run.

## License

MIT
