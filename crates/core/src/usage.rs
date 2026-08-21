//! What a session is costing the machine it runs on.
//!
//! Tokens and dollars are one kind of cost and the transcript has them. This is
//! the other kind: processor and memory, which nothing writes down, so it is
//! measured. A session is a tree of processes — the agent, the shells it spawns,
//! whatever those run — and the number worth showing is the whole tree, since
//! that is what the fan is responding to.
//!
//! Processor time is a rate, so it needs two readings and the gap between them.
//! The first look at a session therefore reports nothing rather than a number
//! made up from a single sample.

use std::collections::HashMap;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// share of one processor, as a percentage; None until measurable
    pub cpu: Option<f64>,
    /// resident bytes across the tree
    pub memory: u64,
    /// how many processes are in the tree
    pub processes: usize,
}

/// One process, as the machine describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proc {
    pub pid: i64,
    pub ppid: i64,
    /// processor ticks used since it started, user and system together
    pub ticks: u64,
    pub rss: u64,
}

/// Sums a process tree, and remembers enough to turn ticks into a rate.
#[derive(Default)]
pub struct Meter {
    last: HashMap<i64, (u64, Instant)>,
}

/// Ticks per second, which is what /proc counts processor time in.
fn hz() -> f64 {
    100.0
}

impl Meter {
    /// What this session's process tree is using now.
    pub fn measure(&mut self, root: i64, table: &[Proc]) -> Usage {
        let tree = descendants(root, table);
        let ticks: u64 = tree.iter().map(|p| p.ticks).sum();
        let memory: u64 = tree.iter().map(|p| p.rss).sum();
        let now = Instant::now();
        let cpu = match self.last.insert(root, (ticks, now)) {
            Some((before, at)) => {
                let seconds = now.duration_since(at).as_secs_f64();
                // Too soon to divide by, and a tree that lost a process would
                // otherwise read as negative work.
                if seconds < 0.2 || ticks < before {
                    None
                } else {
                    Some(((ticks - before) as f64 / hz() / seconds) * 100.0)
                }
            }
            None => None,
        };
        Usage {
            cpu,
            memory,
            processes: tree.len(),
        }
    }

    /// Forget a session, so a machine left running for a week does not keep a
    /// reading for every session it ever had.
    pub fn forget(&mut self, alive: &[i64]) {
        self.last.retain(|pid, _| alive.contains(pid));
    }
}

/// A process and everything below it.
pub fn descendants(root: i64, table: &[Proc]) -> Vec<Proc> {
    let mut out = Vec::new();
    let mut frontier = vec![root];
    let mut seen = std::collections::HashSet::new();
    while let Some(pid) = frontier.pop() {
        if !seen.insert(pid) {
            continue;
        }
        for p in table {
            if p.pid == pid {
                out.push(*p);
            }
            if p.ppid == pid {
                frontier.push(p.pid);
            }
        }
    }
    out
}

/// Every process on the machine, from procfs where there is one.
pub fn table() -> Vec<Proc> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return ps_table();
    };
    let mut out = Vec::new();
    for entry in dir.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i64>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        if let Some(p) = parse_stat(pid, &stat) {
            out.push(p);
        }
    }
    out
}

/// /proc/<pid>/stat, whose second field is a command name that may itself
/// contain spaces and brackets, so everything is read relative to its close.
pub fn parse_stat(pid: i64, stat: &str) -> Option<Proc> {
    let close = stat.rfind(')')?;
    let mut fields = stat[close + 1..].split_whitespace();
    let ppid = fields.nth(1)?.parse().ok()?;
    let utime: u64 = fields.nth(9)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    // Resident pages is field 24, and the cursor is sitting on field 16, so
    // eight are stepped over. One too many lands on the memory *limit*, which
    // is a plausible-looking number and completely wrong.
    let rss_pages: u64 = fields.nth(8)?.parse().unwrap_or(0);
    Some(Proc {
        pid,
        ppid,
        ticks: utime + stime,
        rss: rss_pages * 4096,
    })
}

/// Where there is no procfs, one `ps` call describes everything instead.
fn ps_table() -> Vec<Proc> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-eo", "pid=,ppid=,time=,rss="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_ps_row)
        .collect()
}

/// A `ps` row, whose processor time is written as [[dd-]hh:]mm:ss.
pub fn parse_ps_row(line: &str) -> Option<Proc> {
    let mut f = line.split_whitespace();
    let pid = f.next()?.parse().ok()?;
    let ppid = f.next()?.parse().ok()?;
    let seconds = parse_ps_time(f.next()?)?;
    let rss_kb: u64 = f.next()?.parse().ok()?;
    Some(Proc {
        pid,
        ppid,
        ticks: (seconds * hz()) as u64,
        rss: rss_kb * 1024,
    })
}

fn parse_ps_time(text: &str) -> Option<f64> {
    let (days, rest) = match text.split_once('-') {
        Some((d, rest)) => (d.parse::<f64>().ok()?, rest),
        None => (0.0, text),
    };
    let parts: Vec<f64> = rest
        .split(':')
        .map(|p| p.parse::<f64>().unwrap_or(0.0))
        .collect();
    let clock = match parts.as_slice() {
        [h, m, s] => h * 3600.0 + m * 60.0 + s,
        [m, s] => m * 60.0 + s,
        [s] => *s,
        _ => return None,
    };
    Some(days * 86_400.0 + clock)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: i64, ppid: i64, ticks: u64, rss: u64) -> Proc {
        Proc {
            pid,
            ppid,
            ticks,
            rss,
        }
    }

    #[test]
    fn sums_the_whole_tree_not_just_the_agent() {
        // claude, the shell it spawned, and the command that shell is running.
        let table = vec![
            proc(100, 1, 500, 40 * 1024 * 1024),
            proc(101, 100, 200, 8 * 1024 * 1024),
            proc(102, 101, 50, 2 * 1024 * 1024),
            proc(900, 1, 9999, 999),
        ];
        let mut meter = Meter::default();
        let first = meter.measure(100, &table);
        assert_eq!(first.processes, 3, "the unrelated process is not ours");
        assert_eq!(first.memory, 50 * 1024 * 1024);
        assert!(
            first.cpu.is_none(),
            "a rate needs two readings, and one is not two"
        );
    }

    #[test]
    fn turns_ticks_into_a_rate() {
        let mut meter = Meter::default();
        meter.measure(100, &[proc(100, 1, 0, 0)]);
        // Pretend a second passed by rewinding what was remembered.
        let (ticks, at) = meter.last[&100];
        meter
            .last
            .insert(100, (ticks, at - std::time::Duration::from_secs(1)));
        let usage = meter.measure(100, &[proc(100, 1, 50, 0)]);
        // Fifty ticks of a hundred-a-second clock, in one second: half a core.
        assert_eq!(usage.cpu.unwrap().round(), 50.0);
    }

    #[test]
    fn a_tree_that_shrank_reports_nothing_rather_than_a_negative() {
        let mut meter = Meter::default();
        meter.measure(100, &[proc(100, 1, 900, 0)]);
        let usage = meter.measure(100, &[proc(100, 1, 100, 0)]);
        assert!(usage.cpu.is_none());
    }

    #[test]
    fn reads_a_stat_line_with_a_bracketed_name() {
        // A command name can contain spaces and brackets, which is why the
        // fields are counted from the last close bracket.
        let stat = "4242 (my (odd) name) S 4200 4242 4242 0 -1 4194304 100 0 0 0 \
                    120 30 0 0 20 0 5 0 900 123456 2048 18446744073709551615";
        let p = parse_stat(4242, stat).expect("stat should parse");
        assert_eq!(p.ppid, 4200);
        assert_eq!(p.ticks, 150, "user and system together");
        assert_eq!(p.rss, 2048 * 4096);
    }

    #[test]
    fn reads_the_clock_ps_prints() {
        assert_eq!(parse_ps_time("00:12"), Some(12.0));
        assert_eq!(parse_ps_time("01:30:00"), Some(5400.0));
        assert_eq!(parse_ps_time("2-03:00:00"), Some(183_600.0));
        let row = parse_ps_row(" 4242  4200 00:01:40  2048").unwrap();
        assert_eq!(row.pid, 4242);
        assert_eq!(row.ticks, 10_000);
        assert_eq!(row.rss, 2048 * 1024);
    }
}
