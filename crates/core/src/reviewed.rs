//! Files a person has looked at, and whether they have changed since.
//!
//! This exists because of a button that could not be written. A diff view wants
//! an "accept" next to its "put it back", and accept had nothing to do: the
//! change is already on disk, and a control that changes nothing while implying
//! it blessed something is worse than no control.
//!
//! What it can honestly mean is this. You looked at a file at a particular
//! state and were content with it. That is worth recording, because the thing
//! you actually want afterwards is not a record of your approval — it is to be
//! told when an agent touches that file again. A fleet writing across a hundred
//! files produces a feed nobody can read; a feed that can say "this one you have
//! already read, and this one has moved since" is a different instrument.
//!
//! Kept by absolute path rather than by session. Reviewing is about the file: a
//! second worker editing something you have already read is exactly the case
//! this is for, and keying on the session that happened to be open would miss
//! it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What is known about a file you may have looked at.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Seen {
    /// Nobody has said they read it.
    Never,
    /// Read, and byte for byte what it was then.
    Unchanged { at: i64 },
    /// Read, and something has written to it since.
    Changed { at: i64 },
    /// Read, and it is not there any more.
    Gone { at: i64 },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Mark {
    /// A hash of the contents at the moment it was read.
    fingerprint: u64,
    /// Seconds since the epoch.
    at: i64,
}

#[derive(Debug, Default)]
pub struct Store {
    path: PathBuf,
    marks: BTreeMap<String, Mark>,
}

impl Store {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("reviewed.json");
        let marks = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Store { path, marks }
    }

    fn save(&self) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(&self.marks).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, text).map_err(|e| format!("could not record it: {e}"))
    }

    /// Say that this file has been read, as it stands now.
    pub fn mark(&mut self, file: &Path, now: i64) -> Result<(), String> {
        let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
        self.marks.insert(
            file.to_string_lossy().into_owned(),
            Mark {
                fingerprint: fingerprint(&bytes),
                at: now,
            },
        );
        self.save()
    }

    /// Stop tracking it — used when a change is put back, since the file is
    /// then no longer at the state that was read.
    pub fn forget(&mut self, file: &Path) {
        self.marks.remove(&file.to_string_lossy().into_owned());
        let _ = self.save();
    }

    pub fn state(&self, file: &Path) -> Seen {
        let Some(mark) = self.marks.get(&file.to_string_lossy().into_owned()) else {
            return Seen::Never;
        };
        match std::fs::read(file) {
            Err(_) => Seen::Gone { at: mark.at },
            Ok(bytes) if fingerprint(&bytes) == mark.fingerprint => Seen::Unchanged { at: mark.at },
            Ok(_) => Seen::Changed { at: mark.at },
        }
    }

    /// Every file that has been read, and where it stands now. For a view that
    /// needs to mark up a whole feed without asking once per line.
    pub fn all(&self) -> BTreeMap<String, Seen> {
        self.marks
            .keys()
            .map(|p| (p.clone(), self.state(Path::new(p))))
            .collect()
    }
}

/// FNV-1a, over the whole file.
///
/// Not a cryptographic hash and does not need to be: the question is "has this
/// changed", asked about a file on the machine of the person asking. Size and
/// modification time were the cheaper option and are wrong — an agent rewriting
/// a line to the same length within the same second is exactly the case this
/// has to catch, and it is not a rare one when something is iterating.
fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sightline-reviewed-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_file_nobody_has_read_is_not_a_file_that_changed() {
        let dir = scratch("never");
        let file = dir.join("a.rs");
        std::fs::write(&file, "one").unwrap();
        assert_eq!(Store::load(&dir).state(&file), Seen::Never);
    }

    #[test]
    fn reading_it_and_leaving_it_alone_leaves_it_unchanged() {
        let dir = scratch("unchanged");
        let file = dir.join("a.rs");
        std::fs::write(&file, "one").unwrap();
        let mut s = Store::load(&dir);
        s.mark(&file, 100).unwrap();
        assert_eq!(s.state(&file), Seen::Unchanged { at: 100 });
    }

    #[test]
    fn a_rewrite_of_the_same_length_is_still_a_change() {
        // The reason this hashes contents rather than comparing size and
        // modification time. An agent iterating on a line rewrites it to the
        // same length within the same second constantly; the cheap check calls
        // that unchanged, and the one thing this feature exists to say is that
        // something moved.
        let dir = scratch("same-length");
        let file = dir.join("a.rs");
        std::fs::write(&file, "aaa").unwrap();
        let mut s = Store::load(&dir);
        s.mark(&file, 100).unwrap();
        std::fs::write(&file, "bbb").unwrap();
        assert_eq!(s.state(&file), Seen::Changed { at: 100 });
    }

    #[test]
    fn a_file_that_has_gone_says_so_rather_than_reading_as_changed() {
        // "Changed" invites you to open a diff. There is nothing to open.
        let dir = scratch("gone");
        let file = dir.join("a.rs");
        std::fs::write(&file, "one").unwrap();
        let mut s = Store::load(&dir);
        s.mark(&file, 100).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert_eq!(s.state(&file), Seen::Gone { at: 100 });
    }

    #[test]
    fn what_was_read_survives_being_written_down_and_read_back() {
        let dir = scratch("round-trip");
        let file = dir.join("a.rs");
        std::fs::write(&file, "one").unwrap();
        Store::load(&dir).mark(&file, 100).unwrap();
        assert_eq!(Store::load(&dir).state(&file), Seen::Unchanged { at: 100 });
    }

    #[test]
    fn putting_a_change_back_forgets_that_it_was_read() {
        // The file is no longer at the state somebody was content with, so
        // saying it is unchanged would be a lie in the direction that matters.
        let dir = scratch("forget");
        let file = dir.join("a.rs");
        std::fs::write(&file, "one").unwrap();
        let mut s = Store::load(&dir);
        s.mark(&file, 100).unwrap();
        s.forget(&file);
        assert_eq!(s.state(&file), Seen::Never);
    }
}
