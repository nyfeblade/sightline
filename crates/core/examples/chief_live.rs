//! The smallest version of the thing: one chief, one worker, one real task.
//!
//! Everything in it has to be true at once, which is why it is one example and
//! not five:
//!
//! - a chief runs with no way to start a process, only a tool that asks;
//! - it works out what is needed and assigns it;
//! - the kernel starts the worker, confined, counted, and policed;
//! - the worker does a task whose result is checkable without asking anyone;
//! - the ceiling is real, and a third session is refused by it.
//!
//! Needs a logged-in Claude Code and spends quota, so it is an example rather
//! than a test.
//!
//!     SIGHTLINE_DATA_DIR=/tmp/some-scratch \
//!       cargo run -p sightline-core --example chief_live

use sightline_core::owned;
use sightline_core::{brief, chief, limits, work};
use std::time::{Duration, Instant};

/// The numbers the worker has to add up. Chosen so the answer is not guessable
/// and not a round number: a worker that invented a total rather than reading
/// the file would have to be very lucky.
const NUMBERS: [i64; 12] = [
    4813, 27, 91_402, 5, 66_318, 1_907, 340, 82_215, 12, 45_601, 7_734, 208,
];

fn main() {
    let total: i64 = NUMBERS.iter().sum();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let project = std::env::temp_dir().join(format!("sightline-chief-live-{stamp}"));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("numbers.txt"),
        NUMBERS
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    std::fs::write(
        project.join("README.md"),
        "This project has one outstanding job: numbers.txt holds one integer per\n\
         line, and total.txt should hold their sum and nothing else. total.txt\n\
         does not exist yet.\n",
    )
    .unwrap();

    // A ceiling that is real and small enough to hit: the chief plus one worker
    // is two, so a second worker is the thing the ceiling refuses.
    let ceiling = limits::Limits {
        sessions: Some(2),
        spend: None,
        window: None,
    };
    let where_ = limits::write_machine(&ceiling).expect("could not write the ceiling");
    println!("project  {}", project.display());
    println!("ceiling  {} — {}", ceiling.describe(), where_.display());
    println!("answer   {total} (the worker has to find this out)\n");

    let intent = "Read README.md to find the outstanding job, then get it done. \
                  Assign it to one worker. When the worker says it has finished, \
                  check the file yourself and report what it contains.";
    let opening = chief::brief(
        intent,
        &project.to_string_lossy(),
        brief::Constitution::find(&project).map(|(_, c)| c).as_ref(),
        &ceiling,
        &work::Store::default(),
    );

    // The same definition the front ends start, on purpose: an example that
    // proves a configuration nobody ships proves nothing about the product.
    let spec = chief::spec(None, &opening, &project);

    let started = match owned::start("claude", &project, &spec, Duration::from_secs(5)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("the chief would not start: {e}");
            std::process::exit(1);
        }
    };
    println!("chief    {}\n", started.name);

    let began = Instant::now();
    let mut quiet_since: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let fleet = owned::list();
        let busy = fleet.iter().filter(|o| o.alive && o.busy).count();
        let alive = fleet.iter().filter(|o| o.alive).count();
        print!(
            "\r  {:>3}s · {alive} alive · {busy} busy · {} ",
            began.elapsed().as_secs(),
            fleet
                .iter()
                .map(|o| o.name.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
        use std::io::Write;
        let _ = std::io::stdout().flush();

        // Done when nothing is working and the answer is on disk — or when
        // nothing has been working for long enough that nothing will be.
        let answered = std::fs::read_to_string(project.join("total.txt")).is_ok();
        if busy == 0 {
            let since = *quiet_since.get_or_insert_with(Instant::now);
            if answered && since.elapsed() > Duration::from_secs(8) {
                break;
            }
            if since.elapsed() > Duration::from_secs(90) {
                break;
            }
        } else {
            quiet_since = None;
        }
        if began.elapsed() > Duration::from_secs(600) {
            break;
        }
    }
    println!("\n");

    // ── what actually happened ────────────────────────────────────────────
    let fleet = owned::list();
    let (events, _) = owned::drain();
    let mut decisions: Vec<String> = Vec::new();
    for ev in &events {
        if let sightline_core::bus::Kind::PermissionAnswered { option, by } = &ev.kind {
            let who = match by {
                sightline_core::bus::By::Policy { name } => name.as_str(),
                sightline_core::bus::By::Human => "human",
            };
            decisions.push(format!("{option} [{who}]"));
        }
    }
    let denials = decisions.iter().filter(|d| d.starts_with("deny")).count();

    println!("sessions the kernel started:");
    for o in &fleet {
        println!(
            "   {:<10} {:<8} {}",
            o.name,
            if o.alive { "running" } else { "ended" },
            o.cwd
        );
    }
    println!(
        "\ndecisions at the boundary: {} ({denials} refused)",
        decisions.len()
    );
    let mut counted: std::collections::BTreeMap<&str, usize> = Default::default();
    for d in &decisions {
        *counted.entry(d.as_str()).or_default() += 1;
    }
    for (what, n) in counted {
        println!("   {n:>3} × {what}");
    }

    let answer = std::fs::read_to_string(project.join("total.txt")).unwrap_or_default();
    let got: Option<i64> = answer
        .split_whitespace()
        .next()
        .and_then(|s| s.trim().parse().ok());

    println!();
    check(
        "the chief asked the kernel for a worker",
        fleet.len() >= 2,
        &format!("{} sessions exist; the chief is one of them", fleet.len()),
    );
    check(
        "the worker landed inside the project",
        fleet
            .iter()
            .all(|o| o.cwd.starts_with(&*project.to_string_lossy())),
        "every session's directory is under the project",
    );
    check(
        "the boundary was consulted",
        !decisions.is_empty(),
        &format!("{} decisions", decisions.len()),
    );
    check(
        "the task was actually done",
        got == Some(total),
        &format!("total.txt = {:?}, expected {total}", answer.trim()),
    );

    // The ceiling, asked the way `assign` asks it, with the fleet as it stands.
    let refused = limits::refuse(&ceiling, owned::running(), 0.0);
    check(
        "the ceiling would refuse one more",
        refused.is_some(),
        refused
            .as_deref()
            .unwrap_or("it would have allowed another"),
    );

    let stopped = owned::stop_all();
    println!("\nstopped {}", stopped.join(", "));
    println!("left behind for inspection: {}", project.display());
}

fn check(what: &str, ok: bool, detail: &str) {
    println!(
        "  {} {what}\n      {detail}",
        if ok { "PROVED " } else { "FAILED " }
    );
}
