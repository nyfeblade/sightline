//! Ceilings on what a fleet may do, enforced here rather than trusted to a
//! supervisor.
//!
//! The moment something other than a person is allowed to start sessions, "it
//! will not start too many" stops being a property of the system and becomes a
//! hope about an agent's behaviour. A supervisor told to keep to three workers
//! keeps to three workers until the turn it does not, and by then there are
//! thirty. So the count and the spend are checked at the two doors a session
//! can come through, and a start that would exceed one fails saying which.
//!
//! Where they live matters as much as what they say. The real ceiling is in
//! Ironsight's own directory:
//!
//! ```text
//! ~/.local/share/ironsight/limits.toml
//! ```
//!
//! outside every worktree, because a ceiling a supervised agent can edit is not
//! a ceiling — it is a suggestion in a file it has write access to. A project
//! may add `.ironsight/limits.toml` beside its checks and constitution, but it
//! can only ever be *stricter*: a repository cannot raise the ceiling of the
//! machine it happens to be checked out on.
//!
//! ```toml
//! sessions = 8      # at most this many running at once
//! spend    = 25.0   # dollars, over the window below
//! window   = 24     # hours the spend is measured over
//! ```
//!
//! Nothing here is on by default. A ceiling nobody asked for that refuses a
//! ninth session is a surprise, and surprises are how a tool gets turned off.
//! What is not optional is supervision: `ironsight chief` refuses to start
//! without ceilings in force, because that is exactly the case the doc above is
//! about.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Where a project may lower the machine's ceilings.
pub const FILE: &str = ".ironsight/limits.toml";

/// What is allowed. `None` anywhere means "no ceiling of this kind".
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
pub struct Limits {
    /// How many sessions may be running at once.
    #[serde(default)]
    pub sessions: Option<usize>,
    /// Dollars that may be spent over the window.
    #[serde(default)]
    pub spend: Option<f64>,
    /// Hours the spend is measured over. Absent means the default.
    #[serde(default)]
    pub window: Option<u64>,
}

/// A day, which is the span a person thinks in when they say "it must not spend
/// more than this". Short enough that yesterday's run does not block today's.
pub const DEFAULT_WINDOW_HOURS: u64 = 24;

impl Limits {
    pub fn window_hours(&self) -> u64 {
        self.window.unwrap_or(DEFAULT_WINDOW_HOURS)
    }

    /// Whether anything at all is being limited.
    pub fn any(&self) -> bool {
        self.sessions.is_some() || self.spend.is_some()
    }

    /// How to say it to a person.
    pub fn describe(&self) -> String {
        if !self.any() {
            return "no ceilings are in force".into();
        }
        let mut parts = Vec::new();
        if let Some(n) = self.sessions {
            parts.push(format!("at most {n} sessions running"));
        }
        if let Some(d) = self.spend {
            parts.push(format!(
                "at most ${d:.2} spent in {} hours",
                self.window_hours()
            ));
        }
        parts.join(", ")
    }
}

/// Read one, from wherever it is. A file that will not parse is *not* treated
/// as absent: a ceiling with a typo in it must not silently become no ceiling.
pub fn read(path: &Path) -> Result<Option<Limits>, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    toml::from_str::<Limits>(&text)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Where the machine's ceilings live: outside every worktree, so nothing a
/// supervised session can write to can raise them.
pub fn machine_path() -> PathBuf {
    crate::app::data_dir().join("limits.toml")
}

/// The project's, found the way the checks file is — walking up, because a
/// session may be working in a subdirectory of the repository.
pub fn project_path(from: &Path) -> Option<PathBuf> {
    let mut at = Some(from);
    while let Some(dir) = at {
        let path = dir.join(FILE);
        if path.is_file() {
            return Some(path);
        }
        at = dir.parent();
    }
    None
}

/// Combine the two. A project may lower a ceiling and may never raise one.
///
/// The rule, separated from the world so it can be checked: this is the piece
/// that would let a repository grant itself more than the machine allows, and
/// the only way to be sure it does not is to be able to test it without a
/// filesystem.
pub fn effective(machine: Option<Limits>, project: Option<Limits>) -> Limits {
    let machine = machine.unwrap_or_default();
    let Some(project) = project else {
        return machine;
    };
    Limits {
        sessions: stricter(machine.sessions, project.sessions),
        spend: stricter(machine.spend, project.spend),
        // A longer window counts more spend against the same ceiling, so the
        // longer of the two is the stricter of the two.
        window: match (machine.window, project.window) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        },
    }
}

/// The lower of two ceilings, where absent means no ceiling at all.
fn stricter<T: PartialOrd>(machine: Option<T>, project: Option<T>) -> Option<T> {
    match (machine, project) {
        (Some(a), Some(b)) => Some(if b < a { b } else { a }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// The ceilings in force for work in a directory.
pub fn in_force(cwd: &Path) -> Result<Limits, String> {
    let machine = read(&machine_path())?;
    let project = match project_path(cwd) {
        Some(path) => read(&path)?,
        None => None,
    };
    Ok(effective(machine, project))
}

/// What a start would exceed, if anything.
///
/// Pure, and phrased as a refusal rather than a permission: the caller has to
/// do something with the sentence, so there is no way to consult this and
/// forget to act on it.
pub fn refuse(limits: &Limits, running: usize, spent: f64) -> Option<String> {
    if let Some(most) = limits.sessions {
        // `running` is what is running before this one, so starting makes it
        // one more. A ceiling of three means three, not four.
        if running + 1 > most {
            return Some(format!(
                "that would be {} sessions running and the ceiling is {most} \
                 — raise it in {}",
                running + 1,
                machine_path().display()
            ));
        }
    }
    if let Some(most) = limits.spend {
        if spent >= most {
            return Some(format!(
                "${spent:.2} has been spent in the last {} hours and the ceiling is ${most:.2} \
                 — raise it in {}",
                limits.window_hours(),
                machine_path().display()
            ));
        }
    }
    None
}

/// What the fleet has spent over a window, read from the event journal.
///
/// The journal rather than the session list, deliberately. Spend counted from
/// the sessions currently open is spend you can reset by closing them, which is
/// not a ceiling — it is a speed bump. The journal is what actually happened.
pub fn spent_since(journal: &Path, hours: u64) -> f64 {
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours as i64);
    let total: f64 = crate::bus::replay(journal, 0)
        .iter()
        .filter(|e| e.at >= cutoff)
        .filter_map(|e| match &e.kind {
            crate::bus::Kind::CostSpent { estimate, .. } => Some(*estimate),
            _ => None,
        })
        .sum();
    // Summing nothing gives negative zero, which prints as "-$0.00" and reads
    // as a fleet that has somehow earned money.
    total + 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_may_lower_a_ceiling() {
        let machine = Limits {
            sessions: Some(8),
            spend: Some(25.0),
            window: Some(24),
        };
        let project = Limits {
            sessions: Some(3),
            spend: Some(5.0),
            window: Some(48),
        };
        let it = effective(Some(machine), Some(project));
        assert_eq!(it.sessions, Some(3));
        assert_eq!(it.spend, Some(5.0));
        assert_eq!(
            it.window,
            Some(48),
            "a longer window counts more spend against the same ceiling, so it is stricter"
        );
    }

    #[test]
    fn a_project_may_never_raise_one() {
        // The whole reason the machine's file lives outside every worktree. A
        // repository that asks for more gets what the machine allows.
        let machine = Limits {
            sessions: Some(2),
            spend: Some(1.0),
            window: Some(24),
        };
        let greedy = Limits {
            sessions: Some(100),
            spend: Some(10_000.0),
            window: Some(1),
        };
        let it = effective(Some(machine), Some(greedy));
        assert_eq!(it.sessions, Some(2), "the machine's count stands");
        assert_eq!(it.spend, Some(1.0), "and its spend");
        assert_eq!(
            it.window,
            Some(24),
            "and it cannot shorten the window to make its spend look smaller"
        );
    }

    #[test]
    fn a_project_may_add_a_ceiling_the_machine_did_not_have() {
        let it = effective(
            Some(Limits::default()),
            Some(Limits {
                sessions: Some(3),
                ..Default::default()
            }),
        );
        assert_eq!(it.sessions, Some(3));
        assert_eq!(it.spend, None, "and nothing is invented for the other");
    }

    #[test]
    fn no_files_means_no_ceilings() {
        let it = effective(None, None);
        assert!(!it.any());
        assert_eq!(
            refuse(&it, 500, 9_999.0),
            None,
            "nothing is refused when nobody asked for a ceiling"
        );
    }

    #[test]
    fn the_session_ceiling_counts_the_one_about_to_start() {
        let three = Limits {
            sessions: Some(3),
            ..Default::default()
        };
        assert_eq!(refuse(&three, 2, 0.0), None, "a third is the third");
        let refused = refuse(&three, 3, 0.0).expect("a fourth is refused");
        assert!(
            refused.contains('4') && refused.contains('3'),
            "and it says what it would have been and what is allowed: {refused}"
        );
    }

    #[test]
    fn the_spend_ceiling_refuses_at_the_ceiling_not_past_it() {
        let five = Limits {
            spend: Some(5.0),
            ..Default::default()
        };
        assert_eq!(refuse(&five, 0, 4.99), None);
        let refused = refuse(&five, 0, 5.0).expect("at the ceiling is not under it");
        assert!(
            refused.contains("5.00"),
            "and the ceiling is named: {refused}"
        );
    }

    #[test]
    fn a_ceiling_with_a_typo_in_it_is_not_no_ceiling() {
        // The failure that would matter most: a malformed file read as absent
        // means the fleet runs uncapped, and nothing says so.
        let dir = std::env::temp_dir().join(format!("ironsight-limits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("limits.toml");
        std::fs::write(&path, "sessions = \"three\"\n").unwrap();
        let read = read(&path);
        assert!(
            read.is_err(),
            "a file that will not parse is an error, not an absence: {read:?}"
        );
        std::fs::write(&path, "sessions = 3\n").unwrap();
        assert_eq!(
            read_ok(&path).sessions,
            Some(3),
            "and one that parses is read"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(test)]
    fn read_ok(path: &Path) -> Limits {
        read(path).expect("readable").expect("present")
    }

    #[test]
    fn a_fleet_that_has_spent_nothing_has_spent_nothing() {
        // Summing no numbers in Rust gives negative zero, which formats as
        // "-0.00" and reads as money coming back.
        let nothing = spent_since(Path::new("/nonexistent/events.jsonl"), 24);
        assert_eq!(format!("{nothing:.2}"), "0.00");
    }

    #[test]
    fn a_missing_file_is_simply_no_ceiling() {
        assert_eq!(read(Path::new("/nonexistent/limits.toml")), Ok(None));
    }

    #[test]
    fn spend_is_read_from_what_happened_not_from_what_is_open() {
        use crate::bus::{Bus, Event, Journal, Kind};
        let dir = std::env::temp_dir().join(format!("ironsight-spend-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        {
            let journal = Journal::open(path.clone()).unwrap();
            let mut bus = Bus::new().with_journal(journal);
            for _ in 0..3 {
                bus.publish(Event::new(
                    "s1",
                    "claude",
                    Kind::CostSpent {
                        output: 10,
                        estimate: 1.5,
                    },
                ));
            }
        }
        let spent = spent_since(&path, 24);
        assert!(
            (spent - 4.5).abs() < 1e-9,
            "three turns at $1.50 is $4.50, and closing the session would not change it: {spent}"
        );
        assert_eq!(
            spent_since(&path, 0),
            0.0,
            "a window of nothing counts nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
