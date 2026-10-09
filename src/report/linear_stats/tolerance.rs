//! Named tolerances between the reference definitions and their code (#190
//! step 4).
//!
//! Why: the reference for these metrics is a set of published definitions,
//! and the code that produced the published numbers differs from the text in
//! three places. A comparison that ignored a difference would call a correct
//! tga figure wrong, or a wrong one right. Each difference therefore has a
//! name, and the output carries every figure needed to compare under either
//! reading.
//! What: [`NamedTolerance`] and [`TOLERANCES`], which `tga linear stats`
//! copies into its output.
//! Test: `report::linear_stats::tests::output_names_every_tolerance`.

use serde::Serialize;

/// One difference between a published definition and the code behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct NamedTolerance {
    /// Stable name.
    pub id: &'static str,
    /// The metric it applies to.
    pub metric: &'static str,
    /// What the published definition says; tga's main figure follows it.
    pub published: &'static str,
    /// What the code that produced the published number does.
    pub code: &'static str,
    /// Which output fields carry each reading.
    pub fields: &'static str,
}

/// Late project on its target day.
pub const C11_SAME_DAY_LATE: NamedTolerance = NamedTolerance {
    id: "c11-same-day-late",
    metric: "C11",
    published: "late = completed on a later UTC calendar day than the target date; slip in \
                whole calendar days",
    code: "late = completed any time after midnight UTC of the target date; slip in \
           fractional days",
    fields: "projects.late and projects.late_slip_days (published); \
             projects.late_code_rule and projects.late_code_rule_slip_days (code); \
             projects.late_same_day is the difference",
};

/// Carry-over counts issue slots.
pub const C3_CARRYOVER_SLOTS: NamedTolerance = NamedTolerance {
    id: "c3-carryover-slots",
    metric: "C3",
    published: "carry-over = issue slots: an issue carried through several cycles counts once \
                in each",
    code: "sum over cycles of (last issue count - last completed count) / cycles; the method \
           text said only 'issues open when the cycle closed'. Same numbers",
    fields: "cycles.by_year.*.carryover_issue_slots_per_cycle",
};

/// Bracketed title tags: three groups described, two tested.
pub const C5_TITLE_TAG_GROUPS: NamedTolerance = NamedTolerance {
    id: "c5-title-tag-groups",
    metric: "C5",
    published: "a conforming title starts with three bracketed tags",
    code: "the pattern tests only that a title starts with one bracketed tag followed by '['",
    fields: "title_tags maps a tag count to issues whose title starts with exactly that many \
             bracketed tags: the published count is the sum over 3 and more, the code's count \
             is the sum over 2 and more, plus any title whose second tag is opened but never \
             closed (counted under 1 here)",
};

/// Every named tolerance, in metric order.
pub const TOLERANCES: [NamedTolerance; 3] =
    [C3_CARRYOVER_SLOTS, C5_TITLE_TAG_GROUPS, C11_SAME_DAY_LATE];
