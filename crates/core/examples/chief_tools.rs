//! Proof that a chief is born able to act.
//!
//! The cheap half of `chief_live`, and the half that was wrong in the shipped
//! binary. A chief started from either front end had a brief describing three
//! tools — `assign`, `fleet`, `tell` — and none of them attached, because both
//! front ends built `owned::Spec` themselves and both omitted `kernel_tools`
//! and the policy. A live chief read its brief, went looking, found nothing,
//! and correctly reported the mission undispatchable.
//!
//! Nothing in a unit test could have caught that: the struct was well formed
//! and the flags were consistent with it. What was missing was the composition
//! — Sightline's spec, Claude Code's `--mcp-config`, and a model that can see
//! the result. So this asks a real session to call a real kernel tool, and
//! believes only the tool call.
//!
//! `fleet` on purpose: it is the one kernel tool that reads and starts nothing,
//! so this costs one short turn and no second session.
//!
//!     SIGHTLINE_DATA_DIR=/tmp/some-scratch \
//!       cargo run -p sightline-core --example chief_tools

use sightline_core::owned;
use sightline_core::{chief, limits};
use std::time::{Duration, Instant};

fn main() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let project = std::env::temp_dir().join(format!("sightline-chief-tools-{stamp}"));
    std::fs::create_dir_all(&project).unwrap();

    // The chief's policy consults these on every call, so they have to exist.
    let ceiling = limits::Limits {
        sessions: Some(2),
        spend: None,
        window: None,
    };
    limits::write_machine(&ceiling).expect("could not write the ceiling");

    let opening = "Call the tool that reports every worker in the fleet. \
                   Then say DONE and stop. Do not use any other tool, \
                   do not read any file, and do not explain yourself.";
    let spec = chief::spec(None, opening, &project);

    // Say what is about to be true, so a failure names itself rather than
    // arriving as a timeout with no explanation.
    let argv = owned::argv(&spec);
    println!("project  {}", project.display());
    println!("flags    {}\n", argv.join(" "));

    let started = match owned::start("claude", &project, &spec, Duration::from_secs(5)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("the chief would not start: {e}");
            std::process::exit(1);
        }
    };
    println!("chief    {}", started.name);

    let began = Instant::now();
    let mut called: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    while began.elapsed() < Duration::from_secs(120) {
        let (events, _) = owned::drain();
        for ev in &events {
            match &ev.kind {
                sightline_core::bus::Kind::ToolCalled { tool, .. } => called.push(tool.clone()),
                sightline_core::bus::Kind::ToolFailed { tool, summary } => {
                    failed.push(format!("{tool}: {summary}"))
                }
                _ => {}
            }
        }
        if called.iter().any(|t| t.contains("fleet")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    let _ = owned::stop(&started.name);

    println!(
        "tools    {}",
        if called.is_empty() {
            "(none called)".into()
        } else {
            called.join(", ")
        }
    );
    if !failed.is_empty() {
        println!("failed   {}", failed.join("; "));
    }

    // The verdict, and it is about the kernel tool specifically. A chief that
    // called Read instead has proved only that it can read, which was never in
    // doubt and is not what supervision is.
    if called.iter().any(|t| t.contains("fleet")) {
        println!("\nthe chief reached the kernel. Supervision is attached.");
    } else {
        println!(
            "\nthe chief never reached the kernel — this is the shipped defect, \
             not a flake. Check --mcp-config and --permission-prompt-tool above."
        );
        std::process::exit(1);
    }
}
