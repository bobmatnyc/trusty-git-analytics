//! The input budget every LLM classify prompt is cut to (#178).
//!
//! Why: a commit message of several hundred kilobytes (a pasted diff or a
//! generated file) made Bedrock reject every classify call for that commit
//! as over the model's 200k-token context, so the commit never classified.
//! What: [`PromptBudget::fit`] returns the text the providers send after
//! their fixed prefix: the message and its `llm.context` block unchanged
//! when the whole prompt fits, else both cut at a character boundary, the
//! message first, each cut marked `[truncated N bytes]`. Bedrock, the
//! Anthropic Messages API and the OpenAI-compatible path all send it, so
//! the classify LLM fallback and the complexity backfill share it.
//! Test: `classify::tiers::llm_budget_tests`, `tests` below.

use std::borrow::Cow;

use tracing::warn;

use crate::classify::tiers::llm::LlmClassifier;
use crate::classify::tiers::llm_context::{with_context, CommitContext};
use crate::core::config::{
    default_max_input_tokens_for, LlmSource, LLM_DEFAULT_MAX_INPUT_TOKENS,
    LLM_MIN_INPUT_ROOM_TOKENS,
};

/// Bytes counted per estimated token: one.
///
/// Why: tga has no tokenizer, and an estimate that undercounts lets a
/// prompt through that the provider rejects.
/// What: every token of a byte-level BPE vocabulary (Claude, GPT-4o)
/// decodes to at least one byte, so a text of N bytes is at most N tokens,
/// whatever it holds. Ordinary commit text runs near 3.3 bytes a token
/// (the messages in #178 measured 3.26 characters a token), so the estimate
/// overcounts about threefold; that is the price of a bound that also holds
/// for base64, hex or binary-like text, which tokenizes near one byte a
/// token. The Jev budget relies on the same bound (`jev.rs`).
/// Test: `tests::default_budget_fits_every_default_model`.
pub const BYTES_PER_TOKEN: usize = 1;

/// The text every provider puts before the message (`llm.rs`, `bedrock.rs`).
pub(crate) const USER_PREFIX: &str = "Classify this commit message:\n\n";

/// Tokens kept back for request framing the byte count does not see: the
/// chat template's role markers and message separators.
const FRAMING_TOKENS: usize = 64;

/// The text that opens and closes a truncation marker.
const MARKER_OPEN: &str = "\n[truncated ";
const MARKER_CLOSE: &str = " bytes]";

/// The longest marker: [`MARKER_OPEN`], 20 digits (`u64::MAX`), [`MARKER_CLOSE`].
const MARKER_MAX: usize = MARKER_OPEN.len() + 20 + MARKER_CLOSE.len();

/// The most estimated input tokens one prompt may carry, system prompt
/// included (`llm.max_input_tokens`, #178).
///
/// Invariant: a prompt [`Self::fit`] returns is at most the budget; a
/// budget that leaves less than [`LLM_MIN_INPUT_ROOM_TOKENS`] after the
/// system prompt returns nothing to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptBudget {
    max_input_tokens: usize,
}

impl Default for PromptBudget {
    fn default() -> Self {
        Self::new(LLM_DEFAULT_MAX_INPUT_TOKENS)
    }
}

impl PromptBudget {
    /// A budget of `max_input_tokens` estimated tokens.
    pub fn new(max_input_tokens: usize) -> Self {
        Self { max_input_tokens }
    }

    /// The budget `source` uses when `llm.max_input_tokens` is unset (#178);
    /// see [`default_max_input_tokens_for`].
    pub fn for_source(source: &LlmSource) -> Self {
        Self::new(default_max_input_tokens_for(source))
    }

    /// The budget in estimated tokens.
    pub fn max_input_tokens(&self) -> usize {
        self.max_input_tokens
    }

    /// Bytes left for the message and its block once `system`, the fixed
    /// prefix and the framing allowance are counted.
    fn room(self, system: &str) -> usize {
        let fixed = system.len() + USER_PREFIX.len() + FRAMING_TOKENS * BYTES_PER_TOKEN;
        self.max_input_tokens
            .saturating_mul(BYTES_PER_TOKEN)
            .saturating_sub(fixed)
    }

    /// The text sent after [`USER_PREFIX`] for `message` and `ctx`.
    ///
    /// Why: see the module doc.
    /// What: [`with_context`]'s text, unchanged (borrowed when there is no
    /// block), when it fits the room `system` leaves. Otherwise the block
    /// keeps whatever the message leaves of the room, and at least a quarter
    /// of it; the message gets the rest. A cut part ends in
    /// `\n[truncated N bytes]`, N being the bytes left out, and the result
    /// is a pure function of its inputs. `None` when the room is under
    /// [`LLM_MIN_INPUT_ROOM_TOKENS`]: nothing is sent, so no verdict on a
    /// marker-only prompt is stored.
    /// Test: `llm_budget_tests::oversized_message_fits_the_budget_on_openai_compat`,
    /// `llm_budget_tests::oversized_message_keeps_the_context_block_on_anthropic`,
    /// `llm_budget_tests::oversized_context_block_fits_the_budget`,
    /// `llm_budget_tests::prompt_inside_the_budget_is_byte_identical`,
    /// `tests::bedrock_request_fits_the_budget`,
    /// `tests::room_under_the_floor_sends_nothing`.
    pub(crate) fn fit<'a>(
        self,
        system: &str,
        message: &'a str,
        ctx: Option<&CommitContext>,
    ) -> Option<Cow<'a, str>> {
        let room = self.room(system);
        // #178: a budget the system prompt nearly fills would send only markers.
        if room < LLM_MIN_INPUT_ROOM_TOKENS * BYTES_PER_TOKEN {
            warn!(
                max_input_tokens = self.max_input_tokens,
                system_bytes = system.len(),
                "LLM input budget leaves too little room for the commit; nothing sent (#178)"
            );
            return None;
        }
        let full = with_context(message, ctx);
        if full.len() <= room {
            return Some(full);
        }
        let block = ctx.map(CommitContext::render_plain).unwrap_or_default();
        let block_room = room.saturating_sub(message.len()).max(room / 4);
        let block = cut(&block, block_room);
        let message = cut(message, room.saturating_sub(block.len()));
        let text = format!("{message}{block}");
        debug_assert!(text.len() <= room);
        warn!(
            prompt_bytes = full.len(),
            sent_bytes = text.len(),
            max_input_tokens = self.max_input_tokens,
            "LLM prompt over the input budget; sending it truncated (#178)"
        );
        Some(Cow::Owned(text))
    }
}

/// `text` whole when it is at most `max` bytes; else its longest prefix that
/// ends on a character boundary and leaves room for the marker, followed by
/// `\n[truncated N bytes]`. Below [`MARKER_MAX`] bytes the marker is all
/// that is left.
fn cut(text: &str, max: usize) -> Cow<'_, str> {
    if text.len() <= max {
        return Cow::Borrowed(text);
    }
    let mut end = max.saturating_sub(MARKER_MAX);
    // #178: never split a UTF-8 character; index 0 is always a boundary.
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = text.len() - end;
    Cow::Owned(format!(
        "{}{MARKER_OPEN}{dropped}{MARKER_CLOSE}",
        &text[..end]
    ))
}

impl LlmClassifier {
    /// Cut every prompt this classifier sends to `max_input_tokens`
    /// estimated tokens (#178); see [`PromptBudget`]. `None` keeps the
    /// provider's default, see [`Self::prompt_budget`].
    pub fn with_max_input_tokens(mut self, max_input_tokens: Option<usize>) -> Self {
        self.budget = max_input_tokens.map(PromptBudget::new);
        self
    }

    /// The budget this classifier cuts prompts to (#178): the configured one,
    /// else the default of the source it calls. Bedrock and the Anthropic API
    /// serve 200k-token Claude windows; every other endpoint is treated as
    /// OpenAI-compatible.
    /// Test: `llm_budget_tests::claude_sources_send_150_kb_whole_and_openai_compat_cuts_it`.
    pub fn prompt_budget(&self) -> PromptBudget {
        self.budget.unwrap_or_else(|| {
            PromptBudget::for_source(&match self.provider_label() {
                "bedrock" => LlmSource::Bedrock,
                "anthropic-api" => LlmSource::AnthropicApi,
                _ => LlmSource::Openrouter,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::tiers::llm::SYSTEM_PROMPT;

    /// Why (#178): each default must fit its source's model window with
    /// room for the reply: 128k for `gpt-4o-mini`, 200k for Claude.
    /// What: at one byte a token, OpenAI-compatible sources default to 100k
    /// and Bedrock and the Anthropic API to 190k, which leaves room for the
    /// 2,048-token reply ceiling and the framing allowance.
    /// Test: this test.
    #[test]
    fn default_budget_fits_every_default_model() {
        assert_eq!(BYTES_PER_TOKEN, 1, "tokens <= bytes is the bound relied on");
        let openai = PromptBudget::for_source(&LlmSource::Openrouter).max_input_tokens();
        assert_eq!(openai, PromptBudget::default().max_input_tokens());
        assert_eq!(openai, 100_000);
        assert!(openai + 2_048 + FRAMING_TOKENS <= 128_000, "{openai}");
        for claude in [LlmSource::Bedrock, LlmSource::AnthropicApi] {
            let tokens = PromptBudget::for_source(&claude).max_input_tokens();
            assert_eq!(tokens, 190_000, "{claude:?}");
            assert!(tokens + 2_048 + FRAMING_TOKENS <= 200_000, "{tokens}");
        }
    }

    /// Why (#178): a budget the system prompt nearly fills must send
    /// nothing rather than a marker-only prompt whose verdict is stored.
    /// What: a budget leaving one byte under [`LLM_MIN_INPUT_ROOM_TOKENS`]
    /// of room returns `None`; one leaving exactly that room returns text;
    /// a classifier with such a budget reports a failed call and sends no
    /// request.
    /// Test: this test.
    #[tokio::test]
    async fn room_under_the_floor_sends_nothing() {
        let fixed = SYSTEM_PROMPT.len() + USER_PREFIX.len() + FRAMING_TOKENS;
        let at_floor = PromptBudget::new(fixed + LLM_MIN_INPUT_ROOM_TOKENS);
        assert!(at_floor.fit(SYSTEM_PROMPT, "fix: a", None).is_some());
        let under = PromptBudget::new(fixed + LLM_MIN_INPUT_ROOM_TOKENS - 1);
        assert!(under.fit(SYSTEM_PROMPT, "fix: a", None).is_none());
        assert!(PromptBudget::new(0)
            .fit(SYSTEM_PROMPT, "fix: a", None)
            .is_none());

        let server = wiremock::MockServer::start().await;
        let llm = LlmClassifier::new("m", Some("sk-test".to_string()))
            .with_endpoint(format!("{}/v1/chat/completions", server.uri()))
            .with_max_input_tokens(Some(under.max_input_tokens()));
        let call = llm.classify_detailed("fix: a").await;
        assert_eq!(
            call.outcome,
            crate::classify::tiers::llm_prompt::LlmOutcome::Failed
        );
        assert!(call.verdict.is_none());
        let sent = server.received_requests().await.expect("recording on");
        assert!(sent.is_empty(), "{} requests sent", sent.len());
    }

    /// Why (#178): a byte-count cut must never split a character, at any
    /// offset, and must count the bytes it drops exactly.
    /// What: cuts a 2-, 3- and 4-byte character string at every length
    /// around the marker size; each result is a prefix plus an exact marker
    /// and fits `max`.
    /// Test: this test.
    #[test]
    fn cut_keeps_whole_characters_and_counts_the_dropped_bytes() {
        let text = "é€😀".repeat(40);
        for max in MARKER_MAX..text.len() {
            let out = cut(&text, max);
            assert!(out.len() <= max, "max {max}: {} bytes", out.len());
            let (kept, note) = out.split_once(MARKER_OPEN).expect("marker");
            assert!(text.starts_with(kept));
            assert_eq!(note, format!("{}{MARKER_CLOSE}", text.len() - kept.len()));
        }
        assert!(matches!(cut(&text, text.len()), Cow::Borrowed(_)));
    }

    /// Why (#178): Bedrock needs live AWS for the call itself, so its
    /// request builder is where the cut prompt is proven to fit.
    /// What: the Converse request built from a 1 MiB message's fitted text
    /// carries a system prompt and user message within the budget and the
    /// marker; the same message under a large budget is sent whole.
    /// Test: this test.
    #[test]
    fn bedrock_request_fits_the_budget() {
        let message = "chore: vendored lockfile ".repeat(42_000);
        let budget = PromptBudget::for_source(&LlmSource::Bedrock);
        let text = budget.fit(SYSTEM_PROMPT, &message, None).expect("room");
        let req = crate::classify::tiers::bedrock::converse_request("m", SYSTEM_PROMPT, &text);
        let json = serde_json::to_value(&req).expect("serialize");
        let sent: usize = json["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .map(|m| m["content"].as_str().expect("text").len())
            .sum();
        assert!(sent <= budget.max_input_tokens(), "{sent} bytes sent");
        assert!(text.contains(MARKER_OPEN), "marker present");
        let whole = PromptBudget::new(usize::MAX).fit(SYSTEM_PROMPT, &message, None);
        assert!(matches!(whole, Some(Cow::Borrowed(m)) if m == message));
    }

    /// Why (#178): `llm.max_input_tokens` must reach the classifier, and a
    /// small budget must cut what the default sends whole.
    /// What: `max_input_tokens: 10000` parses from YAML and
    /// `from_llm_config` builds a classifier with that budget; an absent key
    /// keeps the source's default. A 40 KB message fits the default but is
    /// cut to fit 10,000 bytes.
    /// Test: this test.
    #[tokio::test]
    async fn configured_budget_is_the_one_applied() {
        use crate::core::config::LlmConfig;
        use crate::core::creds::CredentialSource;
        let cfg: LlmConfig =
            serde_yaml::from_str("source: openrouter\napi_key_env: K\nmax_input_tokens: 10000\n")
                .expect("parse");
        let unset: LlmConfig = serde_yaml::from_str("source: openrouter\n").expect("parse");
        assert_eq!(unset.max_input_tokens, None);
        let creds = CredentialSource::fixed([("K", "sk-test")]); // pragma: allowlist secret
        let llm = LlmClassifier::from_llm_config_with_creds(&cfg, "m", &creds)
            .await
            .expect("build");
        assert_eq!(llm.prompt_budget(), PromptBudget::new(10_000));
        let unset_cfg = LlmConfig {
            api_key_env: "K".to_string(),
            ..unset
        };
        let llm = LlmClassifier::from_llm_config_with_creds(&unset_cfg, "m", &creds)
            .await
            .expect("build");
        assert_eq!(llm.prompt_budget(), PromptBudget::default());

        let message = "x".repeat(40_000);
        let whole = PromptBudget::default().fit("sys", &message, None);
        assert_eq!(whole.as_deref(), Some(message.as_str()));
        let text = PromptBudget::new(10_000)
            .fit("sys", &message, None)
            .expect("room");
        assert!(
            3 + USER_PREFIX.len() + text.len() <= 10_000,
            "{}",
            text.len()
        );
        let (kept, note) = text.split_once(MARKER_OPEN).expect("marker");
        assert_eq!(note, format!("{}{MARKER_CLOSE}", 40_000 - kept.len()));
    }
}
