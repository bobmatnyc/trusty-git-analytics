//! # tga — trusty-git-analytics
//!
//! Developer productivity analytics from git history. This crate exposes a
//! three-stage pipeline (collect → classify → report) as a library, plus
//! the `tga` binary that drives it.
//!
//! ## Modules
//!
//! - [`core`] — shared types, config, database, error definitions
//! - [`collect`] — Stage 1: git extraction and external-system fetches
//! - [`classify`] — Stage 2: four-tier commit classification cascade
//! - [`report`] — Stage 3: CSV / JSON / Markdown report generation

#![warn(missing_docs)]

pub mod classify;
pub mod collect;
pub mod core;
pub mod report;
