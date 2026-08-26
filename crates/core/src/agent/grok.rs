//! Grok Bot: the Cursor desktop assistant, not a CLI.
//!
//! There is no `grok` program Sightline can PTY-spawn the way it spawns
//! `claude` or `cursor-agent`. This is that assistant — the long-lived chat
//! already running in Cursor — registered as a worker rather than started as
//! one. Assigning creates a connected session; `tell` leaves a message in the
//! mailbox; a later turn reads it with `inbox` over `sightline mcp --as
//! SESSION`, which is the same kernel door Cursor uses. A second door, not a
//! second implementation.
//!
//! How much of the boundary reaches it: some of it, and claiming more would be
//! the thing this file exists to refuse. The door it actually has is Cursor
//! desktop's: `.cursor/hooks.json` and `.cursor/mcp.json`, the same files a
//! Cursor CLI worker gets. Kernel tools (`claim`, `note`, `inbox`) go through
//! `kernel::call`. Native tool calls this assistant makes — the ones Cursor
//! cloud and the desktop agent run themselves — are not a pipe Sightline holds,
//! and are not claimed to stop at the gate. That is Partial, and it is the
//! honest word.

use super::{Adapter, Delivery, Found, Governance, Naming, Options, Record};
use std::path::{Path, PathBuf};

pub struct GrokBot;

impl Adapter for GrokBot {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn label(&self) -> &'static str {
        "Grok Bot"
    }

    fn program(&self) -> &'static str {
        // A name to choose it by, not a binary to run. `which("grok")` is
        // false on a machine that has this assistant, and that is the point
        // of `present`.
        "grok"
    }

    fn command(&self, _options: Options) -> Vec<String> {
        Vec::new()
    }

    fn resume(&self, _id: &str) -> Option<Vec<String>> {
        None
    }

    fn spawnable(&self) -> bool {
        false
    }

    fn delivery(&self) -> Delivery {
        Delivery::Mailbox
    }

    fn present(&self) -> bool {
        // The desktop assistant, not a binary. Cursor IDE on the path, or this
        // process *is* that assistant (`CURSOR_AGENT` is how Cursor says so).
        std::env::var_os("CURSOR_AGENT").is_some()
            || super::which("cursor")
            || super::which("Cursor")
    }

    fn naming(&self) -> Naming {
        Naming::Kept
    }

    fn record(&self) -> Record {
        Record::None
    }

    fn conversations(&self, _roots: &[PathBuf]) -> Vec<Found> {
        Vec::new()
    }

    fn install_hint(&self) -> Option<&'static str> {
        Some(
            "Grok Bot is the Cursor desktop assistant, not a CLI. Open a chat in Cursor; \
             work assigned to `grok` waits in the mailbox and is read with the inbox tool \
             (`sightline mcp --as SESSION`).",
        )
    }

    fn signin_hint(&self) -> Option<&'static str> {
        None
    }

    fn signin_probe(&self) -> Option<(&'static [&'static str], &'static str)> {
        None
    }

    fn governance(&self) -> Governance {
        Governance::Partial
    }

    fn governance_note(&self) -> &'static str {
        "partly governed — kernel tools and Cursor desktop hooks stop at the boundary \
         when this workspace is prepared; this assistant is not a process Sightline \
         holds, so it cannot prove every native tool call stops here"
    }

    fn prepare(&self, root: &Path, sightline: &Path) -> Result<(), String> {
        // The same files Cursor CLI gets, because this *is* Cursor's desktop
        // assistant: hooks.json is the permission door it actually has.
        crate::agent::cursor::put_boundary(root, sightline)
    }

    fn offer_kernel(&self, root: &Path, session: &str, sightline: &Path) -> Result<(), String> {
        crate::agent::cursor::put_kernel(root, session, sightline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{self, Adapter};

    #[test]
    fn it_is_known_by_the_id_a_route_would_write() {
        assert_eq!(agent::find("grok").unwrap().label(), "Grok Bot");
        assert!(
            !agent::find("grok").unwrap().spawnable(),
            "there is no grok binary to start"
        );
        assert_eq!(
            agent::find("grok").unwrap().delivery(),
            agent::Delivery::Mailbox
        );
    }

    #[test]
    fn it_does_not_claim_a_boundary_it_does_not_hold() {
        let g = GrokBot;
        assert_eq!(g.governance(), Governance::Partial);
        assert!(
            !g.governance_note().contains("every call"),
            "Full's sentence must not be borrowed: {}",
            g.governance_note()
        );
    }

    #[test]
    fn preparing_a_workspace_writes_the_door_that_exists() {
        let dir =
            std::env::temp_dir().join(format!("sightline-grok-prepare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        GrokBot
            .prepare(&dir, std::path::Path::new("/usr/bin/sightline"))
            .unwrap();
        GrokBot
            .offer_kernel(&dir, "owned-4", std::path::Path::new("/usr/bin/sightline"))
            .unwrap();
        let hooks = std::fs::read_to_string(dir.join(".cursor/hooks.json")).unwrap();
        let mcp = std::fs::read_to_string(dir.join(".cursor/mcp.json")).unwrap();
        assert!(hooks.contains("preToolUse"), "{hooks}");
        assert!(mcp.contains("owned-4"), "{mcp}");
        assert!(mcp.contains("\"mcp\""), "{mcp}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
