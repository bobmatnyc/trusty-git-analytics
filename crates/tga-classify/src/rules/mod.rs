//! Rule loading and types for the classification cascade.
//!
//! Rules are loaded from YAML or JSON files (detected by extension) into a
//! [`types::RuleSet`]. A built-in [`loader::default_rules`] covers
//! conventional commit prefixes and common categories so the cascade works
//! without an external rules file.

pub mod loader;
pub mod types;

pub use loader::{default_rules, load_rules};
pub use types::{Rule, RuleSet};
