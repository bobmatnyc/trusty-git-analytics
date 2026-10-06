//! Validation of a Jev reply (#111).
//!
//! Why: every reply is untrusted input from a third party; one function
//! decides whether it becomes a verdict, so a malformed or unexpected reply
//! can only ever be `failed`. Split out of `jev.rs` for the size cap.
//! What: [`interpret`] turns a decoded reply into an [`LlmCall`].
//! Test: `classify::tiers::jev_tests`.

use std::collections::BTreeMap;

use serde::Deserialize;
use tracing::warn;

use super::jev::{ABSTAIN_CODES, JEV_MODEL, QUESTION};
use crate::classify::tiers::llm_prompt::{LlmCall, LlmUsage};
use crate::classify::tiers::ClassificationResult;
use crate::core::models::ClassificationMethod;

#[derive(Deserialize)]
struct JevResponse {
    answers: BTreeMap<String, JevAnswer>,
}

#[derive(Deserialize)]
struct JevAnswer {
    #[serde(rename = "type")]
    kind: String,
    choice: String,
    probabilities: BTreeMap<String, f64>,
}

#[derive(Deserialize)]
struct JevUsage {
    input_tokens: u64,
    output_tokens: u64,
}

/// `usage` from a reply, or `None` when it is missing or malformed.
fn parse_usage(reply: &serde_json::Value) -> Option<LlmUsage> {
    let u: JevUsage = serde_json::from_value(reply.get("usage")?.clone()).ok()?;
    Some(LlmUsage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
    })
}

/// Turn a decoded reply into an [`LlmCall`].
///
/// Why: fail-closed validation in one place, and the owner pins the model
/// (#111): a reply served by another model is not the model that was
/// evaluated.
/// What: no usage → `failed`, carrying the reply's model when it named
/// one. No non-empty `model` → `failed`. A `model`
/// other than [`JEV_MODEL`] → `failed`, carrying the served id so
/// `llm_usage` records it. Then a malformed envelope, a non-`choice`
/// answer, probabilities outside `[0, 1]`, a mass far from 1, or a choice
/// whose probability trails the maximum → `failed`. Otherwise either
/// abstain code (`NO_MATCH`, `INSUFFICIENT_INFORMATION`) → `abstained`; a
/// code outside `codes` → `out_of_set`; a known code → `answered` with its
/// real category and confidence equal to its probability. Every call that
/// got a reply naming a model carries that model id.
/// Test: `jev_tests::abstain_codes_are_abstentions`,
/// `jev_tests::unknown_code_is_out_of_set`,
/// `jev_tests::answer_confidence_is_the_chosen_probability`,
/// `jev_tests::invalid_probabilities_fail_closed`,
/// `jev_tests::reply_without_model_fails`,
/// `jev_tests::reply_from_another_model_is_a_recorded_failure`,
/// `jev_review_tests::reply_without_usage_records_its_model`.
pub(super) fn interpret(reply: serde_json::Value, codes: &BTreeMap<String, String>) -> LlmCall {
    // #111: read the model first, so every path records what served it.
    let named = reply
        .get("model")
        .and_then(|m| m.as_str())
        .filter(|m| !m.trim().is_empty())
        .map(str::to_string);
    let Some(usage) = parse_usage(&reply) else {
        warn!("Jev reply has no valid usage block");
        let call = LlmCall::failed(None);
        return match named {
            Some(m) => call.with_model(m),
            None => call,
        };
    };
    let usage = Some(usage);
    let Some(model) = named else {
        warn!("Jev reply names no model");
        return LlmCall::failed(usage);
    };
    if model != JEV_MODEL {
        warn!(served = %model, pinned = JEV_MODEL, "Jev reply came from an unpinned model");
        return LlmCall::failed(usage).with_model(model);
    }
    let call = |c: LlmCall| c.with_model(model.clone());
    let Ok(mut parsed) = serde_json::from_value::<JevResponse>(reply) else {
        warn!("Jev reply does not match the decision envelope");
        return call(LlmCall::failed(usage));
    };
    let Some(answer) = parsed.answers.remove(QUESTION) else {
        warn!("Jev reply has no answer to the category question");
        return call(LlmCall::failed(usage));
    };
    let probs = &answer.probabilities;
    let valid = |p: f64| p.is_finite() && (0.0..=1.0).contains(&p);
    let mass: f64 = probs.values().sum();
    let max = probs.values().copied().fold(0.0_f64, f64::max);
    // Returned probabilities may be rounded; tolerate cumulative rounding.
    if answer.kind != "choice"
        || probs.is_empty()
        || !probs.values().all(|&p| valid(p))
        || (mass - 1.0).abs() > 0.005 * probs.len() as f64 + 0.001
    {
        warn!("Jev reply carries an invalid probability distribution");
        return call(LlmCall::failed(usage));
    }
    if ABSTAIN_CODES.contains(&answer.choice.as_str()) {
        return call(LlmCall::abstained(usage));
    }
    let Some(category) = codes.get(&answer.choice) else {
        warn!("Jev chose a code outside the configured set; abstaining");
        return call(LlmCall::out_of_set(usage));
    };
    match probs.get(&answer.choice) {
        Some(&p) if p + 0.011 >= max => call(LlmCall::answered(
            ClassificationResult {
                category: category.clone(),
                subcategory: None,
                top_level: None,
                confidence: p,
                method: ClassificationMethod::LlmFallback,
                ticket_id: None,
                complexity: None,
            },
            usage,
        )),
        _ => {
            warn!("Jev choice disagrees with its probabilities");
            call(LlmCall::failed(usage))
        }
    }
}
