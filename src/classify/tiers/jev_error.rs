//! Errors of the Jev provider and its pseudonymizer (#111).
//!
//! Why: every Jev failure must stop the message it concerns before anything
//! is sent, and the caller must be able to tell why without the message
//! text. A closed `thiserror` enum makes each cause explicit and keeps
//! commit text out of every message.
//! What: [`JevError`]. No variant carries commit text; a category name
//! (operator config) is the only free text any variant holds.
//! Test: `jev_obfuscate_tests::matcher_build_error_is_an_error`,
//! `jev_obfuscate_tests::failed_rebuild_poisons_the_run`,
//! `jev_tests::obfuscation_error_sends_nothing`.

use thiserror::Error;

/// Why a Jev message, or the whole Jev run, could not be sent.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum JevError {
    /// A built-in detection pattern failed to compile.
    #[error("jev pseudonymizer pattern `{0}` does not compile; nothing was sent")]
    Pattern(&'static str),
    /// An operator `llm.jev.id_patterns` entry is not a valid regex.
    #[error("llm.jev.id_patterns[{0}] is not a valid regex; nothing was sent")]
    IdPattern(usize),
    /// The name matcher could not be built.
    #[error("jev pseudonymizer: the matcher over {count} names {why}; nothing was sent")]
    Matcher {
        /// Names the matcher had to cover.
        count: usize,
        /// `exceeds N bytes` or `is invalid`.
        why: String,
    },
    /// An earlier matcher rebuild failed, so names the run has seen may be
    /// unmatched; every later message fails closed.
    #[error("jev pseudonymizer is disabled for the rest of the run after a failed rebuild; nothing was sent")]
    Poisoned,
    /// The category set is empty or larger than Jev accepts.
    #[error("jev needs 1 to {max} categories, got {got}")]
    CategoryCount {
        /// The largest category set allowed.
        max: usize,
        /// The configured set's size.
        got: usize,
    },
    /// A category name repeats or equals an abstain code.
    #[error("jev category '{0}' is duplicated or reserved")]
    CategoryName(String),
    /// `llm.model` names a model other than the pinned one.
    #[error("llm.model '{0}' is not supported for source jev; tga pins {pinned}", pinned = super::jev::JEV_MODEL)]
    UnpinnedModel(String),
    /// No API key outside payload-dump mode.
    #[error("jev source requires an API key")]
    MissingKey,
    /// The HTTP client could not be built.
    #[error("jev HTTP client could not be built: {0}")]
    Client(String),
}
