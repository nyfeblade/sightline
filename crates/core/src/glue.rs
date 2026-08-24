//! Reconciling a fork onto a new upstream release, by teaching the fork's own
//! agent rather than by merging text.
//!
//! The problem this exists for: people fork Ironsight, customise it, and then
//! every release pulls them further out of step. `git merge` matches line
//! numbers and file paths, knows nothing about what a module is for, and hands
//! back conflict markers for a human to resolve — so in practice nobody
//! resolves them, and the fork stops updating.
//!
//! The other half of the situation is that whoever forked it already has an
//! agent, and that agent already knows their fork. What it does not know is
//! upstream: the layers, the seams a customisation is meant to live in, the
//! invariants that must survive a merge, and how upstream tests. That is
//! knowledge the author has and the fork's agent does not, and it is the same
//! for every fork — so it is written down once, shipped with the binary, and
//! installed into the fork.
//!
//! ```text
//!     the ability          what upstream knows, encoded once
//!         +
//!     the divergence       what actually changed, computed per fork
//!         +
//!     the fork's agent     what it already knows about the fork
//!         ↓
//!     an owned session in a worktree, gated by the project's own checks
//! ```
//!
//! Nothing here is clever. The reconciliation is done by an agent, in a
//! worktree, and the result counts only when the project's checks pass and its
//! refutations do not fire — which is the same bar every other piece of work in
//! Ironsight has to clear. An agent's own confidence that the merge is fine is
//! worth exactly what it is worth everywhere else in this codebase, which is
//! nothing.
//!
//! What this module is careful about is that it will run inside *someone
//! else's* repository, which may be laid out nothing like this one. Every step
//! that can fail says what it needed and carries on or stops cleanly. It never
//! writes to the fork's working tree, never commits, and never touches a branch
//! a person is standing on.

use std::collections::BTreeSet;
use std::path::Path;

/// The ability, carried in the binary so a fork can be taught without fetching
/// anything. Compiled in rather than read from disk: the copy that matters is
/// the one that shipped with this version of Ironsight, and a file next to the
/// binary is a file that can be edited to say something upstream never said.
pub const ABILITY: &str = include_str!("../../../.claude/skills/ironsight-glue/SKILL.md");

/// What the ability is called, and where a fork keeps it.
pub const ABILITY_NAME: &str = "ironsight-glue";

/// Where it installs to, relative to the fork's root.
pub fn ability_path(root: &Path) -> std::path::PathBuf {
    root.join(".claude")
        .join("skills")
        .join(ABILITY_NAME)
        .join("SKILL.md")
}

/// Install the ability into a fork, so the fork's own agent can use it whether
/// or not anyone ever runs `ironsight glue`.
///
/// Returns where it went. Overwrites deliberately: the ability is upstream's
/// and a stale copy is worse than none, because it describes seams that have
/// since moved.
pub fn install(root: &Path) -> Result<std::path::PathBuf, String> {
    let path = ability_path(root);
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent to write into", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(&path, ABILITY).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// How far a fork has drifted, in the only terms that matter for a merge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Divergence {
    /// Files upstream changed since the two parted company.
    pub upstream: Vec<String>,
    /// Files the fork changed since then.
    pub fork: Vec<String>,
    /// Files both changed. This is the work; everything else is mechanical.
    pub contested: Vec<String>,
}

impl Divergence {
    /// The three sets, from the two lists of changed paths.
    ///
    /// Pure, because this is the part worth being sure about and the part that
    /// would otherwise need two repositories to test. Sorted and deduplicated:
    /// `git` can list a path twice across a rename, and a contested file listed
    /// twice reads as two problems.
    pub fn of(upstream: &[String], fork: &[String]) -> Divergence {
        let up: BTreeSet<&String> = upstream.iter().collect();
        let mine: BTreeSet<&String> = fork.iter().collect();
        Divergence {
            upstream: up.iter().map(|s| (*s).clone()).collect(),
            fork: mine.iter().map(|s| (*s).clone()).collect(),
            contested: up.intersection(&mine).map(|s| (*s).clone()).collect(),
        }
    }

    /// Whether there is anything to reconcile at all.
    pub fn quiet(&self) -> bool {
        self.upstream.is_empty()
    }

    /// A fork that has changed nothing is not a fork, and a plain `git merge`
    /// will do the job without any of this.
    pub fn untouched(&self) -> bool {
        self.fork.is_empty()
    }
}

/// Ask git for the three sets.
///
/// Every failure here is somebody else's repository being shaped differently
/// from this one, so each says what it wanted rather than what went wrong
/// internally.
pub fn divergence(repo: &Path, upstream_ref: &str) -> Result<Divergence, String> {
    let base = run(
        repo,
        &["merge-base", "HEAD", upstream_ref],
        "find where the fork and upstream parted company",
    )?;
    let base = base.trim();
    if base.is_empty() {
        return Err(format!(
            "HEAD and {upstream_ref} share no history — is {upstream_ref} really the \
             upstream this fork came from?"
        ));
    }
    let upstream = changed(repo, base, upstream_ref)?;
    let fork = changed(repo, base, "HEAD")?;
    Ok(Divergence::of(&upstream, &fork))
}

fn changed(repo: &Path, from: &str, to: &str) -> Result<Vec<String>, String> {
    let out = run(
        repo,
        &["diff", "--name-only", &format!("{from}..{to}")],
        &format!("list what changed between {from} and {to}"),
    )?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Run a git command, and say what was being attempted if it will not.
fn run(repo: &Path, args: &[&str], attempting: &str) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("could not run git to {attempting}: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.trim();
        return Err(format!(
            "git could not {attempting}{}",
            if why.is_empty() {
                String::new()
            } else {
                format!(" — {why}")
            }
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether a ref exists in this repository.
pub fn known(repo: &Path, reference: &str) -> bool {
    run(
        repo,
        &["rev-parse", "--verify", "--quiet", reference],
        "resolve a ref",
    )
    .map(|s| !s.trim().is_empty())
    .unwrap_or(false)
}

/// Which remote is upstream.
///
/// A fork usually has `upstream` pointing at where it came from and `origin`
/// pointing at its own copy. Where there is no `upstream`, `origin` is the only
/// honest guess, and the caller is told which was used rather than left to
/// assume.
pub fn upstream_remote(repo: &Path) -> Option<String> {
    let out = run(repo, &["remote"], "list the remotes").ok()?;
    let remotes: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if remotes.iter().any(|r| *r == "upstream") {
        return Some("upstream".into());
    }
    remotes.first().map(|r| (*r).to_string())
}

/// Whether the working tree has changes that a reconciliation would sit on top
/// of. Not fatal, but the caller must say so: uncommitted work is the thing
/// most easily lost in a merge, and the fork's own agent will be editing files.
pub fn dirty(repo: &Path) -> bool {
    run(repo, &["status", "--porcelain"], "read the working tree")
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// How many contested files to name before saying "and N more".
///
/// A cap rather than the whole list, because a brief that is mostly a file
/// listing buries the part that matters. It is said out loud when it bites —
/// silent truncation reads as "that was all of them".
const NAMED: usize = 40;

/// The packet a reconciling session opens on.
///
/// It carries the facts the agent would otherwise spend three turns collecting,
/// and points at the ability for everything that is the same for every fork.
/// The ability is not inlined: it is installed as a skill in the fork, so the
/// agent reads it the way it reads any other skill, and it stays available
/// after this session ends.
pub fn brief(
    version: &str,
    upstream_ref: &str,
    fork_root: &str,
    worktree: &str,
    divergence: &Divergence,
    checks: Option<&str>,
    dirty: bool,
) -> String {
    let mut out = String::new();

    out.push_str(
        "You are reconciling this fork of Ironsight onto a newer upstream release.\n\n\
         Read the `ironsight-glue` skill first. It is upstream's own account of the\n\
         architecture, the seams a customisation is meant to live in, the invariants\n\
         that must survive a merge, and how upstream tests. You know this fork; that\n\
         skill is what you do not know.\n\n",
    );

    out.push_str(&format!(
        "RECONCILING\n\
         \x20 fork      {fork_root}\n\
         \x20 onto      {version}  ({upstream_ref})\n\
         \x20 worktree  {worktree}\n\n"
    ));

    out.push_str(&format!(
        "WHAT DIVERGED\n\
         \x20 upstream changed {} file(s) since you parted company\n\
         \x20 this fork changed {}\n\
         \x20 both changed {} — this is the work\n\n",
        divergence.upstream.len(),
        divergence.fork.len(),
        divergence.contested.len()
    ));

    if divergence.contested.is_empty() {
        out.push_str(
            "\x20 Nothing is contested. Every upstream change lands in a file this fork\n\
             \x20 never touched, so this should merge cleanly — but check that no upstream\n\
             \x20 change moved something this fork depends on, because a file the fork did\n\
             \x20 not edit can still be a file the fork calls.\n\n",
        );
    } else {
        out.push_str("CONTESTED FILES\n");
        for path in divergence.contested.iter().take(NAMED) {
            out.push_str(&format!("\x20 {path}\n"));
        }
        if divergence.contested.len() > NAMED {
            out.push_str(&format!(
                "\x20 … and {} more, not listed here. Get the full list with:\n\
                 \x20   git diff --name-only {upstream_ref}...HEAD\n",
                divergence.contested.len() - NAMED
            ));
        }
        out.push('\n');
    }

    match checks {
        Some(file) => out.push_str(&format!(
            "THE GATE\n\
             \x20 This project defines what done means in {file}. Run it with\n\
             \x20 `ironsight check` from the worktree, and run upstream's own suite too:\n\
             \n\
             \x20   cargo fmt --check\n\
             \x20   cargo test\n\
             \x20   node crates/gui/ui/tokenize.test.mjs\n\
             \x20   cargo check --target x86_64-pc-windows-msvc -p ironsight-core -p ironsight\n\n"
        )),
        None => out.push_str(
            "THE GATE\n\
             \x20 This project has no .ironsight/checks.toml, so there is nothing\n\
             \x20 mechanical to hold the merge to and the result can only ever be\n\
             \x20 unverified. Run upstream's suite anyway:\n\
             \n\
             \x20   cargo fmt --check\n\
             \x20   cargo test\n\
             \x20   node crates/gui/ui/tokenize.test.mjs\n\
             \n\
             \x20 and say plainly in your report that nothing verified this.\n\n",
        ),
    }

    if dirty {
        out.push_str(
            "BEFORE YOU START\n\
             \x20 The fork's working tree had uncommitted changes when this began. You are\n\
             \x20 in a worktree of your own so they are not in front of you, but they are\n\
             \x20 also not part of what you are reconciling. Say so in your report.\n\n",
        );
    }

    out.push_str(
        "WHAT TO DO\n\
         \x20 Follow the protocol in the skill. In short: classify every contested\n\
         \x20 change, keep upstream's structure and reapply this fork's behaviour inside\n\
         \x20 it, write the test that would have failed without each adapter, run the\n\
         \x20 gate, and report.\n\
         \n\
         \x20 Work only in the worktree above. Do not commit to the fork's own branch,\n\
         \x20 and do not merge anything — leave the result on the worktree branch for a\n\
         \x20 person to take.\n\
         \n\
         \x20 If you hit an upstream change to one of the invariants in the skill, stop\n\
         \x20 and report it rather than reconciling it.\n",
    );

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_contested_set_is_what_both_sides_touched() {
        let d = Divergence::of(
            &v(&["core/app.rs", "core/bus.rs", "README.md"]),
            &v(&["core/app.rs", "gui/ui/app.js"]),
        );
        assert_eq!(
            d.contested,
            v(&["core/app.rs"]),
            "only the file both changed is work"
        );
        assert_eq!(d.upstream.len(), 3);
        assert_eq!(d.fork.len(), 2);
    }

    #[test]
    fn a_path_listed_twice_is_one_problem_not_two() {
        // git will list a path more than once across a rename, and a contested
        // file named twice reads as two things to reconcile.
        let d = Divergence::of(
            &v(&["core/app.rs", "core/app.rs"]),
            &v(&["core/app.rs", "core/app.rs"]),
        );
        assert_eq!(d.contested, v(&["core/app.rs"]));
        assert_eq!(d.upstream, v(&["core/app.rs"]));
    }

    #[test]
    fn nothing_upstream_is_nothing_to_do() {
        let d = Divergence::of(&[], &v(&["core/app.rs"]));
        assert!(d.quiet(), "upstream changed nothing, so there is no merge");
        assert!(!d.untouched());
    }

    #[test]
    fn a_fork_that_changed_nothing_needs_none_of_this() {
        let d = Divergence::of(&v(&["core/app.rs"]), &[]);
        assert!(d.untouched(), "a plain merge would do");
        assert!(d.contested.is_empty());
    }

    #[test]
    fn the_brief_says_what_is_contested_and_where_to_work() {
        let d = Divergence::of(
            &v(&["crates/core/src/app.rs", "README.md"]),
            &v(&["crates/core/src/app.rs"]),
        );
        let out = brief(
            "v0.5.0",
            "upstream/v0.5.0",
            "/w/fork",
            "/w/fork/../glue-v0.5.0",
            &d,
            Some(".ironsight/checks.toml"),
            false,
        );
        assert!(out.contains("crates/core/src/app.rs"), "the contested file");
        assert!(out.contains("v0.5.0"), "what it is being reconciled onto");
        assert!(out.contains("glue-v0.5.0"), "and where to do the work");
        assert!(
            out.contains("ironsight-glue"),
            "and that the knowledge is in the skill: {out}"
        );
        assert!(
            out.contains("do not merge") || out.contains("Do not commit"),
            "and that it does not get to merge its own work"
        );
    }

    #[test]
    fn a_project_with_no_checks_is_told_its_result_cannot_be_verified() {
        // The failure this guards: a clean-looking merge reported as done, on a
        // fork where nothing mechanical ever ran.
        let out = brief(
            "v0.5.0",
            "upstream/v0.5.0",
            "/w",
            "/w2",
            &Divergence::default(),
            None,
            false,
        );
        assert!(
            out.contains("unverified"),
            "it says the result cannot be verified: {out}"
        );
    }

    #[test]
    fn uncommitted_work_in_the_fork_is_mentioned_rather_than_swept_up() {
        let out = brief(
            "v0.5.0",
            "upstream/v0.5.0",
            "/w",
            "/w2",
            &Divergence::default(),
            None,
            true,
        );
        assert!(out.contains("uncommitted"), "{out}");
    }

    #[test]
    fn a_very_wide_divergence_says_what_it_left_out() {
        // Silent truncation reads as "that was all of them", which would have
        // an agent reconcile forty files and report the job done.
        let many: Vec<String> = (0..NAMED + 5).map(|i| format!("file{i}.rs")).collect();
        let d = Divergence::of(&many, &many);
        let out = brief("v0.5.0", "upstream/v0.5.0", "/w", "/w2", &d, None, false);
        assert!(
            out.contains("and 5 more"),
            "the ones not listed are counted out loud: {out}"
        );
        assert!(
            out.contains("git diff --name-only"),
            "and it says how to see them all"
        );
    }

    #[test]
    fn the_ability_is_carried_in_the_binary() {
        // A fork can be taught with nothing fetched, and the copy that teaches
        // it is the one that shipped with this version.
        assert!(ABILITY.contains("ironsight-glue"), "it is the right file");
        assert!(
            ABILITY.contains("Invariants") || ABILITY.contains("invariant"),
            "and it carries the part that cannot be worked out from the diff"
        );
        assert!(
            ABILITY.len() > 2_000,
            "and it is the whole document, not a stub"
        );
    }

    #[test]
    fn installing_it_puts_it_where_an_agent_will_look() {
        let dir = std::env::temp_dir().join(format!("ironsight-glue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = install(&dir).expect("it installs into a fresh fork");
        assert!(path.ends_with("SKILL.md"));
        assert!(
            path.to_string_lossy()
                .contains(".claude/skills/ironsight-glue"),
            "where Claude Code looks for a skill: {}",
            path.display()
        );
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, ABILITY, "and it is the shipped copy, verbatim");

        // Again, over a stale copy: an old ability describes seams that have
        // moved, which is worse than none.
        std::fs::write(&path, "something older").unwrap();
        install(&dir).expect("it overwrites");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), ABILITY);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_that_is_not_a_repository_is_told_so_rather_than_panicking() {
        // This runs inside someone else's project, which may be laid out
        // nothing like this one. Every step that can fail says what it wanted.
        let dir = std::env::temp_dir().join(format!("ironsight-notrepo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = divergence(&dir, "v1.0.0");
        assert!(out.is_err(), "it does not pretend to have an answer");
        let why = out.unwrap_err();
        assert!(
            why.contains("parted company") || why.contains("git could not"),
            "and it says what it was trying to do: {why}"
        );
        assert!(!known(&dir, "v1.0.0"));
        assert_eq!(upstream_remote(&dir), None);
        assert!(!dirty(&dir), "and nothing here reads as unsaved work");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
