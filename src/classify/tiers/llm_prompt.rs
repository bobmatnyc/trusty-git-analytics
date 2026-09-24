//! LLM-tier prompt construction, reply validation and token accounting.
//!
//! Why (#131): with `extend_defaults: false` the rules file defines the whole
//! category set, so the LLM must be offered only those categories plus an
//! explicit abstain option, and any reply outside that set must be dropped
//! rather than stored. #111 needs every call's token usage to price a full run.
//! What: [`restricted_system_prompt`] builds the prompt; [`resolve`] turns a
//! provider reply into an [`LlmCall`] (verdict, usage, outcome), shared by the
//! HTTP and Bedrock paths.
//! Test: `classify::tiers::llm_prompt_tests`.

use tracing::warn;

use crate::classify::rules::CategoryDef;
use crate::classify::tiers::llm::LlmVerdict;
use crate::classify::tiers::ClassificationResult;
use crate::core::models::ClassificationMethod;

/// The abstain label offered to the LLM; it matches the eval's no-answer label.
pub const ABSTAIN_LABEL: &str = "unclear";

/// Complexity scale, identical to the one in the default `SYSTEM_PROMPT`.
const COMPLEXITY_GUIDE: &str = "Complexity 1-5: \
1=trivial (config/version bump/typo), 2=simple (single-file bugfix), \
3=moderate (multi-file feature), 4=complex (cross-module/arch change), \
5=highly complex (system design/major refactor).";

/// What happened on one LLM call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmOutcome {
    /// A verdict the pipeline may adopt.
    Answered,
    /// The model chose the abstain label.
    Abstained,
    /// The model named a category outside the configured set; dropped.
    OutOfSet,
    /// No usable reply: transport error, non-2xx, refusal, or unparseable text.
    Failed,
}

impl LlmOutcome {
    /// Stable label stored in `llm_usage.outcome`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Abstained => "abstained",
            Self::OutOfSet => "out_of_set",
            Self::Failed => "failed",
        }
    }
}

/// Token usage the provider reported for one call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LlmUsage {
    /// Prompt tokens billed.
    pub input_tokens: u64,
    /// Completion tokens billed, thinking included.
    pub output_tokens: u64,
}

/// One LLM call's result.
#[derive(Debug, Clone)]
pub struct LlmCall {
    /// `Some` only when `outcome == Answered`.
    pub verdict: Option<ClassificationResult>,
    /// `None` when the provider reported no usage (e.g. a transport error).
    pub usage: Option<LlmUsage>,
    /// How the call ended.
    pub outcome: LlmOutcome,
}

impl LlmCall {
    /// A call that produced nothing usable.
    pub fn failed(usage: Option<LlmUsage>) -> Self {
        Self {
            verdict: None,
            usage,
            outcome: LlmOutcome::Failed,
        }
    }
}

/// System prompt offering only `categories` plus [`ABSTAIN_LABEL`].
///
/// Why: see the module doc. A category's `description` is shown when the
/// rules file gives one; otherwise the name stands alone.
/// What: one line per category, the abstain line, then the JSON contract and
/// complexity scale. No example commit text is included.
/// Test: `llm_prompt_tests::restricted_prompt_lists_only_configured_categories`.
pub fn restricted_system_prompt(categories: &[CategoryDef]) -> String {
    let mut lines = String::from(
        "You are a git commit classifier. Assign the commit to exactly one of these categories:\n",
    );
    for c in categories {
        match c
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            Some(d) => {
                let flat = d.split_whitespace().collect::<Vec<_>>().join(" ");
                lines.push_str(&format!("- {}: {flat}\n", c.name));
            }
            None => lines.push_str(&format!("- {}\n", c.name)),
        }
    }
    lines.push_str(&format!(
        "- {ABSTAIN_LABEL}: none of the categories above clearly fits, or the message \
         is too vague to decide.\n"
    ));
    let names: Vec<&str> = categories
        .iter()
        .map(|c| c.name.as_str())
        .chain(std::iter::once(ABSTAIN_LABEL))
        .collect();
    lines.push_str(&format!(
        "Respond with ONLY a JSON object: {{\"category\": \"<one of: {}>\", \
         \"subcategory\": null, \"confidence\": 0.0-1.0, \"complexity\": <integer 1-5>}}. \
         {COMPLEXITY_GUIDE} No prose, no markdown.",
        names.join("|")
    ));
    lines
}

/// Parse the JSON verdict out of a model reply.
///
/// Tolerates a markdown fence or a sentence around the object by taking the
/// span from the first `{` to the last `}`.
fn parse_verdict(text: &str) -> Option<LlmVerdict> {
    let t = text.trim();
    let span = match (t.find('{'), t.rfind('}')) {
        (Some(a), Some(b)) if a < b => &t[a..=b],
        _ => t,
    };
    serde_json::from_str(span)
        .map_err(|e| warn!(error = %e, "LLM verdict JSON parse failed"))
        .ok()
}

/// Turn a provider reply into an [`LlmCall`].
///
/// Why: fail-closed validation in one place for every provider (#131).
/// What: no text or unparseable text → `Failed`. With `allowed` set, the
/// abstain label → `Abstained`; a category matching a configured name
/// (case-insensitive) → `Answered` under the configured spelling with no
/// subcategory; anything else → `OutOfSet`, never stored. With `allowed`
/// unset the verdict is kept as the model gave it (pre-#131 behaviour).
/// Test: `llm_prompt_tests::out_of_set_reply_is_an_abstention`,
/// `llm_prompt_tests::abstain_label_is_an_abstention`,
/// `llm_prompt_tests::unrestricted_reply_is_kept`.
pub fn resolve(
    text: Option<&str>,
    allowed: Option<&[CategoryDef]>,
    usage: Option<LlmUsage>,
) -> LlmCall {
    let Some(verdict) = text.and_then(parse_verdict) else {
        return LlmCall::failed(usage);
    };
    let mut category = verdict.category.trim().to_string();
    let mut subcategory = verdict.subcategory;
    if let Some(allowed) = allowed {
        if category.eq_ignore_ascii_case(ABSTAIN_LABEL) {
            return LlmCall {
                verdict: None,
                usage,
                outcome: LlmOutcome::Abstained,
            };
        }
        match allowed
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&category))
        {
            Some(c) => {
                category = c.name.clone();
                // #131: the prompt asks for no subcategory; one the model
                // invents is outside the configured taxonomy too.
                subcategory = None;
            }
            None => {
                warn!(category = %category, "LLM category outside the configured set; abstaining");
                return LlmCall {
                    verdict: None,
                    usage,
                    outcome: LlmOutcome::OutOfSet,
                };
            }
        }
    }
    LlmCall {
        verdict: Some(ClassificationResult {
            category,
            subcategory,
            top_level: None, // resolved by ClassificationEngine via the taxonomy registry
            confidence: verdict.confidence.clamp(0.0, 1.0),
            method: ClassificationMethod::LlmFallback,
            ticket_id: None,
            complexity: verdict.complexity.map(|v| v.clamp(1, 5)),
        }),
        usage,
        outcome: LlmOutcome::Answered,
    }
}
