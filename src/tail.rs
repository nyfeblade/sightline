//! Incremental reader for an append-only JSONL file.
//!
//! Claude Code appends to the transcript while a turn is in flight, so a poll
//! can land mid-line and mid-UTF-8-sequence. Leftover bytes are carried to the
//! next poll rather than decoded early.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

pub struct Tail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl Tail {
    pub fn new(path: PathBuf) -> Self {
        Tail {
            path,
            offset: 0,
            partial: Vec::new(),
        }
    }

    /// Start reading from `offset` instead of the top. Used to cap how much
    /// history a very large transcript replays at startup.
    pub fn skip_to(&mut self, offset: u64) {
        self.offset = offset;
        self.partial.clear();
    }

    /// Complete lines appended since the previous poll.
    pub fn poll(&mut self) -> std::io::Result<Vec<String>> {
        let len = std::fs::metadata(&self.path)?.len();
        if len < self.offset {
            // File was rewritten (fork, compact). Re-read from the top.
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset {
            return Ok(Vec::new());
        }
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::with_capacity((len - self.offset) as usize);
        let read = f.take(len - self.offset).read_to_end(&mut buf)?;
        self.offset += read as u64;
        self.partial.extend_from_slice(&buf);

        let mut lines = Vec::new();
        let mut start = 0;
        for i in 0..self.partial.len() {
            if self.partial[i] == b'\n' {
                let seg = &self.partial[start..i];
                if !seg.is_empty() {
                    lines.push(String::from_utf8_lossy(seg).into_owned());
                }
                start = i + 1;
            }
        }
        self.partial.drain(..start);
        Ok(lines)
    }
}
