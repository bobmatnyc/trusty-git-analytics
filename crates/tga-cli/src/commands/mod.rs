//! Subcommand implementations for the `tga` binary.
//!
//! Each module exposes a single `run` function invoked by `main.rs` after
//! the CLI is parsed and the database is opened.

pub mod analyze;
pub mod classify;
pub mod collect;
pub mod report;
