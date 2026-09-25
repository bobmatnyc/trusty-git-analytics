//! How a profiling pass hands its JSON Schema to the model (#5588).
//!
//! Why: a schema rendered as prose in the system turn is a request the model may
//! decline — #5588 was filed because the drift is observable as unparseable
//! output. `trusty_common::inference::ChatRequest::response_schema` constrains
//! the completion instead, but only where the provider can honour it: Bedrock's
//! Converse API has no schema parameter, and `tga profile --model bedrock/…` is
//! a documented invocation. Sending the field there would be
//! `InferenceError::UnsupportedCapability` before the socket opens, turning
//! every period of a Bedrock run into a skip.
//! What: [`deliver_schema`] picks one delivery per call — the real field when
//! the adapter supports structured output, the prose fallback when it does not
//! — so exactly one copy of the schema reaches the model either way.
//! [`sampling_temperature`] is the other per-provider fork both passes share:
//! whether the request may carry a `temperature` at all (#111).
//! Test: `schema_delivery_tests.rs`.

use serde_json::Value;
use trusty_common::inference::{ProviderId, StructuredOutput};

/// The `temperature` one profiling pass may send to `model`.
///
/// Why (#111): Claude Sonnet 5 on Bedrock rejects any request that sets
/// `temperature`, so no `bedrock/…` request carries one, whatever the model.
/// Other providers keep the pass's fixed temperature.
/// What: `None` when `model` routes to Bedrock, by the same
/// [`ProviderId::from_slug_prefix`] rule `provider_for` routes on (Bedrock
/// always resolves at that stage); `Some(temperature)` otherwise.
/// Test: `bedrock_slugs_get_no_temperature`.
pub(crate) fn sampling_temperature(model: &str, temperature: f32) -> Option<f32> {
    match ProviderId::from_slug_prefix(model) {
        Some(ProviderId::Bedrock) => None,
        _ => Some(temperature),
    }
}

/// Choose how one pass's schema reaches the model.
///
/// Why: both profiling passes (period review and narrative synthesis) face the
/// same fork, and a second copy of it would be the duplication #5588 is about.
/// The fallback rather than a refusal is tga's fail-open precedent: a provider
/// failure already leaves a period skipped or the narrative deterministic
/// (`PeriodReview::skipped`, `apply_fallback_narrative`) rather than aborting
/// the run, and a schema the provider cannot enforce is still a schema the model
/// can be asked to follow.
/// What: with `structured_output` true, returns `base_system` unchanged plus
/// `Some(StructuredOutput)` for the caller to put on
/// [`trusty_common::inference::ChatRequest::response_schema`]. With it false,
/// returns `base_system` with the schema appended as a fenced JSON block and
/// `None`. `strict` is left at [`StructuredOutput::new`]'s default — the passes
/// want the constraint, not best-effort JSON.
/// Test: `structured_provider_gets_the_field_and_a_clean_system_turn`,
/// `unsupported_provider_gets_the_prose_fallback`,
/// `the_schema_is_delivered_exactly_once`.
pub fn deliver_schema(
    base_system: &str,
    name: &str,
    schema: Value,
    structured_output: bool,
) -> (String, Option<StructuredOutput>) {
    if structured_output {
        return (
            base_system.to_string(),
            Some(StructuredOutput::new(name, schema)),
        );
    }

    // `to_string_pretty` over a `Value` the caller built cannot fail; an empty
    // string would still leave the prose instructions in the system turn.
    let rendered = serde_json::to_string_pretty(&schema).unwrap_or_default();
    let system = format!(
        "{base_system}\n\n## Response schema\nReturn ONLY a JSON object conforming to this \
         schema:\n```json\n{rendered}\n```"
    );
    (system, None)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "schema_delivery_tests.rs"]
mod tests;
