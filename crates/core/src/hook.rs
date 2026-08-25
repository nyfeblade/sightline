//! Sightline's boundary, reached the way Cursor reaches one.
//!
//! Claude Code hands a permission decision to a tool the host serves. Cursor
//! does the same thing by a different route: it spawns a command, writes the
//! call to its standard input as JSON, and reads the answer back from its
//! standard output. Different plumbing, same question — so the same kernels
//! answer it, and `gate::decide` is called here exactly as it is called there.
//!
//! That matters more than it sounds. The alternative is a second boundary with
//! a second set of rules, which is two things to keep true and one of them
//! silently wrong. There is one gate in this program; this is another door into
//! the same room.
//!
//! The contract was read out of Cursor's own binary rather than its
//! documentation, which does not mention hooks at all:
//!
//! ```text
//! executeHookForStep(preToolUse, {…, tool_name, tool_input, tool_use_id})
//! new PreToolUseRequestResponse({ permission, userMessage, agentMessage,
//!                                 updatedInput, additionalContext })
//! if ("deny" === i?.permission) { throw … }
//! ```
//!
//! `permission` takes `allow`, `deny` or `ask`, and `updated_input` replaces the
//! call's arguments. Those are the same four answers the gate already gives —
//! allow, deny, rewrite, and hand it to a person — which is why this is a
//! translation and not a design.

use crate::gate::{self, Decision, Policy};
use serde_json::{Value, json};
use std::path::PathBuf;

/// What Cursor asked, reduced to what a kernel needs to know.
struct Asked {
    session: String,
    tool: String,
    input: Value,
    root: PathBuf,
}

/// Read one hook request, decide it, and write the answer.
///
/// Every failure here is an allow, deliberately, and it is worth being explicit
/// about why: this process sits in front of every tool call a session makes, and
/// a hook that cannot parse its input would otherwise stop the session dead. A
/// boundary that fails closed on its own bugs is a boundary nobody leaves
/// switched on. The kernels fail closed; the plumbing around them does not.
pub fn answer(request: &str) -> String {
    let Some(asked) = read(request) else {
        return json!({ "permission": "allow" }).to_string();
    };
    let policy = Policy::confined_to(&asked.root);
    let (decision, kernel) = gate::decide(&policy, &asked.session, &asked.tool, &asked.input);
    reply(decision, kernel).to_string()
}

fn read(request: &str) -> Option<Asked> {
    let v: Value = serde_json::from_str(request).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();

    // The workspace, in the order Cursor offers it. `workspace_roots` is what a
    // worktree-confined session is actually confined to; `cwd` is where the call
    // happens to be running.
    let root = v
        .get("workspace_roots")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| {
            let cwd = s("cwd");
            (!cwd.is_empty()).then(|| PathBuf::from(cwd))
        })?;

    let session = match s("session_id") {
        id if !id.is_empty() => id,
        _ => s("conversation_id"),
    };

    // Two shapes arrive here. `preToolUse` names a tool and carries its
    // arguments; `beforeShellExecution` carries a bare command, because a shell
    // call is not a tool call to Cursor. The kernels only know tools, so a shell
    // becomes one — and it becomes `Bash`, because that is the name the forbid
    // and trust kernels were written against and a second spelling would be a
    // rule that silently stops applying.
    let (tool, input) = match v.get("hook_event_name").and_then(Value::as_str) {
        Some("beforeShellExecution") | Some("afterShellExecution") => (
            "Bash".to_string(),
            json!({ "command": s("command"), "description": "" }),
        ),
        _ => {
            let name = s("tool_name");
            if name.is_empty() {
                return None;
            }
            (
                name,
                v.get("tool_input").cloned().unwrap_or_else(|| json!({})),
            )
        }
    };

    Some(Asked {
        session,
        tool,
        input,
        root,
    })
}

fn reply(decision: Decision, kernel: &str) -> Value {
    match decision {
        Decision::Allow => json!({ "permission": "allow" }),
        // The reason goes to the model as well as to the person. A refusal a
        // model cannot read is a refusal it will make again in a moment.
        Decision::Deny { why } => json!({
            "permission": "deny",
            "user_message": format!("Sightline refused this ({kernel}): {why}"),
            "agent_message": why,
        }),
        // Allowed, but not as asked. This is the answer with no equivalent in a
        // settings file, and the reason a gate that can only say yes or no has
        // to escalate every ambiguous call to a person.
        Decision::Rewrite { input, why } => json!({
            "permission": "allow",
            "updated_input": input,
            "agent_message": why,
            "user_message": format!("Sightline amended this ({kernel}): {why}"),
        }),
    }
}

/// The file Cursor reads to find this.
///
/// Written into a worktree rather than installed globally, so a session governed
/// by Sightline is one Sightline started somewhere it prepared — and a Cursor
/// somebody runs by hand is untouched. Governing a person's own editor because
/// they once used this program would be a surprise, and the wrong kind.
pub fn config(sightline: &std::path::Path) -> String {
    let bin = sightline.to_string_lossy();
    serde_json::to_string_pretty(&json!({
        "version": 1,
        "hooks": {
            // Every call, before it happens. This one covers the file edits that
            // `beforeShellExecution` cannot see, which was the whole gap.
            "preToolUse": [{ "command": format!("{bin} hook") }],
            // Shell is not a tool call to Cursor, so it arrives by its own door.
            "beforeShellExecution": [{ "command": format!("{bin} hook") }],
            "beforeMCPExecution": [{ "command": format!("{bin} hook") }],
        }
    }))
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(request: Value) -> Value {
        serde_json::from_str(&answer(&request.to_string())).unwrap()
    }

    #[test]
    fn a_forbidden_command_is_refused_before_it_runs() {
        let out = ask(json!({
            "hook_event_name": "beforeShellExecution",
            "session_id": "cursor-1",
            "workspace_roots": ["/tmp/sightline-hook-test"],
            "command": "git push --force origin main",
        }));
        assert_eq!(out["permission"], "deny");
        assert!(
            out["agent_message"].as_str().unwrap_or("").len() > 10,
            "the model is told why, or it tries again in a moment"
        );
    }

    #[test]
    fn an_ordinary_command_is_allowed() {
        let out = ask(json!({
            "hook_event_name": "beforeShellExecution",
            "session_id": "cursor-1",
            "workspace_roots": ["/tmp/sightline-hook-test"],
            "command": "cargo test",
        }));
        assert_eq!(out["permission"], "allow");
    }

    #[test]
    fn a_write_outside_the_worktree_is_answered_by_the_scope_kernel() {
        // The gap that made Cursor only partly governable: its file edits have
        // no `beforeShellExecution` to stop them. `preToolUse` does see them,
        // which is what closes it.
        let out = ask(json!({
            "hook_event_name": "preToolUse",
            "session_id": "cursor-1",
            "workspace_roots": ["/tmp/sightline-hook-test"],
            "tool_name": "Write",
            "tool_input": { "file_path": "/etc/passwd", "content": "x" },
        }));
        assert_ne!(
            out["permission"], "deny_not_reached",
            "the scope kernel must have had an opinion"
        );
        assert!(
            out["permission"] == "deny" || out.get("updated_input").is_some(),
            "a write outside the root is refused or redirected, never simply allowed: {out}"
        );
    }

    #[test]
    fn a_request_this_does_not_understand_lets_the_call_through() {
        // This process sits in front of every call a session makes. A parser bug
        // here must not stop the session: the kernels fail closed, the plumbing
        // around them fails open, and that is what keeps it switched on.
        assert_eq!(ask(json!({ "nonsense": true }))["permission"], "allow");
        let broken: Value = serde_json::from_str(&answer("not json")).unwrap();
        assert_eq!(broken["permission"], "allow");
    }

    #[test]
    fn the_config_points_at_this_binary_and_covers_the_edits() {
        let text = config(std::path::Path::new("/usr/local/bin/sightline"));
        assert!(text.contains("preToolUse"), "{text}");
        assert!(text.contains("/usr/local/bin/sightline hook"));
    }
}
