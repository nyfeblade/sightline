//! Everything Ironsight knows how to do, with no opinion about how it is shown.
//!
//! The terminal view and the desktop app are both front ends over this: they
//! ask the same questions and call the same actions, so behaviour cannot drift
//! between them. Anything that would have to be written twice belongs here.

pub mod agent;
pub mod app;
pub mod bootstrap;
pub mod control;
pub mod event;
pub mod git;
pub mod history;
// Selected only on Windows, but built and tested everywhere, so on Unix its
// surface is unused by design.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod host;
pub mod notify;
pub mod pricing;
pub mod registry;
pub mod screen;
pub mod session;
pub mod tail;
#[cfg(not(windows))]
pub mod tmux;
pub mod usage;
