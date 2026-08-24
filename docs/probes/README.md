# Probes

Experiments against a real Claude Code, kept in the repository because the
architecture rests on what they show and the first version of them was written
in a scratch directory and was gone the next day.

They are not tests and are not in `checks.toml`: they need a logged-in Claude
Code, they spend quota, and they take a couple of minutes. Run one when you are
about to design around a behaviour, or when a Claude Code release might have
moved it.

    python3 docs/probes/control_protocol.py all
    python3 docs/probes/control_protocol.py deny rewrite

Each prints PROVED or FAILED against a property that can actually be wrong.
`deny` and `rewrite` are the two that matter most: an approval path that cannot
refuse, or cannot amend what it approves, is decoration.

Last run 2026-08-24 against Claude Code 2.1.241 on a Claude Max subscription:
6/6 proved. Findings are written up in `docs/ARCHITECTURE.md`.
