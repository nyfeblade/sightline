//! Everything Sightline knows how to do, with no opinion about how it is shown.
//!
//! The terminal view and the desktop app are both front ends over this: they
//! ask the same questions and call the same actions, so behaviour cannot drift
//! between them. Anything that would have to be written twice belongs here.

pub mod agent;
pub mod app;
pub mod backdrop;
pub mod bootstrap;
pub mod brief;
pub mod bus;
pub mod checks;
pub mod chief;
pub mod control;
pub mod daemon;
pub mod event;
pub mod gate;
pub mod gateway;
pub mod git;
pub mod glue;
pub mod history;
pub mod hook;
// Selected only on Windows, but built and tested everywhere, so on Unix its
// surface is unused by design.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod host;
pub mod kernel;
pub mod ladder;
pub mod limits;
pub mod mail;
pub mod mcp;
pub mod notify;
pub mod owned;
pub mod pricing;
pub mod redact;
pub mod registry;
pub mod reviewed;
pub mod routing;
pub mod screen;
pub mod session;
pub mod stream;
pub mod tail;
#[cfg(not(windows))]
pub mod tmux;
pub mod usage;
pub mod work;
