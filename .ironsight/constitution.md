# Ironsight

## Mission
Be the layer between a person and a fleet of coding agents: know what every
session is doing, act on any of them, and give whatever supervises them a shared
view of reality instead of each one guessing separately.

## Architecture
One engine (`ironsight-core`) with two front ends over it — a terminal view and
a desktop app — neither holding logic the other lacks. Capability grows through
layers and adapters, never by adding features to the core.

## Constraints
- Every layer must be useful as the top layer; dependencies point downward only.
- Prove it against something real: unit tests are necessary and never sufficient.
- Pin other people's formats with a fixture and a test that names what moved.
- Keep the failure honest: say "cannot tell" rather than guessing.
- [gui] No external scripts, fonts, or assets; the window's CSP forbids them.
- [redact] Anything that leaves — the journal, the socket — is redacted; the
  interface still shows the real command.
- [daemon] The daemon owns pseudo-terminals and nothing else; judgement stays in
  the front ends.

## Preferences
- Plainer prose over clever prose, in code comments and in the interface.
- Colour reserved for state, so when something changes colour it means something.

## Rejected approaches
- A monolith — it loses everything at once when a format changes.
- Ironsight becoming an agent, a model provider, or a judge of quality.

## Definition of done
- The change is proved against something real, not only unit-tested.
- The compatibility suite covers any format it now depends on.
- `cargo fmt --check`, the tests, and the Windows cross-check are clean.

## Open questions
- Does a supervised organisation of agents produce better software, for less
  human attention, than the same person directing the same agents by hand?
