//! Report formatters: CSV, JSON, Markdown.
//!
//! Each submodule exposes one or more `write_*` functions that take a
//! reference to [`crate::report::models::ReportData`] and an output directory and
//! return the path of the file written.

pub mod csv;
pub mod json;
pub mod markdown;
