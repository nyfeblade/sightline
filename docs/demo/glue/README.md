# Watching glue reconcile a fork

    docs/demo/glue/run.sh

Two scripts. `setup.sh` builds a fork that has genuinely diverged from its
upstream; `run.sh` walks you through what happens to it.

    ./run.sh            build it, and see what a plain git merge does
    ./run.sh --brief    also print the packet the reconciling agent is opened with
    ./run.sh --start    also start the reconciliation — this runs a real agent

Everything lives in `/tmp/glue-demo`, with a scratch Sightline state directory at
`/tmp/glue-demo-state`. Your own fleet, session list and settings are untouched.

## The scenario

A small notifier. Upstream renamed its only function and changed what it
returns. The fork, meanwhile, patched that same file with a mute switch and
built three functions on the old name, every one of which tags its messages with
the estate they came from.

That shape is chosen so the interesting failure is visible. `git merge` stops
with a conflict in the file both sides changed, which you would expect and would
deal with. What you would not notice is that resolving it the obvious way — take
upstream's version of the file — silently removes the mute switch. No test
upstream ever wrote covers it. The fork's own test for it went out with the
conflict. The build break gets your attention; the missing feature does not.

That is what glue is pointed at: not the conflict, the thing lost while
resolving it.

## What the fork writes down

`.sightline/checks.toml` says what finished means — its test suite — and, beside
it, two `[[invariant]]` entries: commands that must *fail*, written to succeed
only when a guarantee has stopped being true. One says every message carries the
estate tag. The other says the mute switch survives. A merge that drops either
is caught by something mechanical rather than by somebody noticing weeks later.

`.sightline/constitution.md` says why, in the fork's own words, and the
reconciling agent is given it.

## What to watch

Step 5 starts an agent in a worktree of its own and prints the command to watch
it with. In another terminal:

    SIGHTLINE_DATA_DIR=/tmp/glue-demo-state sightline

The session appears in the list with its assignment. Open it and the Talk view
shows the brief it was opened with and everything it does. When it goes idle,
judge it from the worktree — the checks say whether it built, the invariants say
whether it kept what the fork could not afford to lose.

Nothing is merged. The result sits on its own branch for you to take or throw
away, which is the point: glue does not get to decide the merge worked.

## One honest note

The `sightline-glue` ability that ships in the binary is Sightline's own account
of itself — its layers, its seams, its invariants. About half of it is the
general method (how to classify an upstream change, how to reconcile without
losing either side, what counts as done, when to stop and ask) and applies to any
fork. The other half describes Sightline specifically and does not apply to this
little Python project.

This demo is therefore showing you the mechanism rather than a perfect fit: the
divergence, the containment, the brief, and the gate. For a fork of Sightline
itself the whole ability lands. For anything else, that document is the part an
upstream would write for its own project.
