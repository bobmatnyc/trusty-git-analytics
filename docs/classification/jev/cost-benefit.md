# Cost-benefit analysis: rules vs. LLM tiers ([#111](https://github.com/bobmatnyc/trusty-git-analytics/issues/111))

Figures below come from the 100-commit subsample in
[`README.md`](README.md) §3.

**Method.** All runs use the same 100-row sample, the same rater-1 labels
and the same scorer: `tga eval score` with buckets. 76 rows are scored.
Accuracy is weighted by stratum, with 95% confidence intervals. The
accuracy column below is fine-category accuracy. Primary (bucket) and
secondary accuracy are in [`README.md`](README.md) §3.

The four LLM arms (Haiku 4.5, Sonnet 5, Sonnet 5.5, Jev) have overlapping
intervals on every measure. At n = 76 they are statistically level. Jev
abstains more (11 of 32 routed rows) but is the most precise when it
answers: 17/21 (81%), against 20/26 (77%) for Sonnet 5.5.

"Routed" means a commit the rules left
unanswered and that was sent to the LLM tier; "overall" scales that per-call
cost by the share of non-merge commits actually routed.

## Options compared

| Option | Accuracy (weighted, 95% CI) | Unanswered share | Cost / 1,000 LLM-routed commits | Cost / 1,000 commits overall | 26-week window estimate | Who sees commit text | Operational needs |
|---|---|---|---|---|---|---|---|
| Rules only (v2 + precision rules) | 32.9% [20.3, 45.6] — measured | 30/80 (37.5%) of the labelled subsample; 51.4% of all stored commits get a rule-based category, so ~48.6% do not | none — no LLM call | $0 | $0 | No third party; classification runs locally against the operator's own database | None beyond the rules file already in the config |
| Rules + Bedrock Claude Haiku 4.5 | **45.9% [32.8, 58.9] — measured** | 4/80 (5%) of the subsample; 20.7% of non-merge commits are routed to the LLM under the current rules | $0.68 | $0.14 | ≈ $6.41 | AWS Bedrock, in the operator's AWS account and region: AWS processes the text under the operator's AWS agreement; per Bedrock's data terms, the model provider does not receive it | `cargo build --features bedrock`; AWS credentials via the default chain or `AWS_PROFILE`; Bedrock model access enabled for the account's region |
| Rules + Bedrock Claude Sonnet 5 | **47.1% [34.1, 60.1] — measured** | 4/80 (5%) of the subsample, same as Haiku 4.5; the overall and 26-week figures reuse the documented 20.7% routed share, not independently re-measured for Sonnet 5 | $2.01 | $0.42 | ≈ $18.92 | AWS Bedrock, in the operator's AWS account and region: AWS processes the text under the operator's AWS agreement; per Bedrock's data terms, the model provider does not receive it | Same as Haiku; on Bedrock, needs tga at or after the [#140](https://github.com/bobmatnyc/trusty-git-analytics/pull/140) merge commit, which drops `temperature` from every Bedrock request — an earlier version sends it and Sonnet 5 rejects the call. On OpenRouter, Sonnet 5 accepts `temperature: 0.0` directly (verified 2026-09-25); only Bedrock needed the fix |
| Rules + Bedrock Claude Sonnet 5.5 (`us.anthropic.claude-sonnet-5-5`) | **47.1% [34.1, 60.1] — measured** | same routed population as the other arms; overall and 26-week figures reuse the documented 20.7% routed share | $2.13 | $0.44 | ≈ $19.8 | AWS Bedrock, in the operator's AWS account and region: AWS processes the text under the operator's AWS agreement; per Bedrock's data terms, the model provider does not receive it | Same as Sonnet 5 |
| Rules + Jev (jev-1.13.0, TypeSafe's hosted model, real commit text) | **45.0% [32.0, 58.1] — measured** | 32 routed rows: 21 adopted, 11 abstained, 0 failed; the overall and 26-week figures reuse the documented 20.7% routed share | $0.0338 | $0.0070 | ≈ $0.32 | TypeSafe's service, over the network, when `llm.jev.obfuscate` is `false` (the default). Set `llm.jev.obfuscate: true` to pseudonymize the text first | A Jev API key, read from the env var named by `api_key_env`; `llm.source: jev` |

## Price basis

- **Haiku 4.5 on Bedrock:** $1.00 / $5.00 per million input / output tokens
  (Bedrock on-demand, US East (N. Virginia), read 2026-09-25 from the
  [AWS Bedrock pricing page](https://aws.amazon.com/bedrock/pricing/)). The
  measured run used 48 LLM calls, 24,171 input tokens and 1,736 output
  tokens (about 504 input and 36 output tokens per call).
- **Sonnet 5 on Bedrock:** $2.20 / $11.00 per million input / output tokens,
  same source and date — about 2.2x Haiku's per-token price. The measured
  run used 32 LLM calls (28 adopted, 4 abstained, 0 out-of-set, 0 failed),
  23,321 input tokens and 1,197 output tokens (about 729 input and 37 output
  tokens per call). Sonnet 5's LLM-eligible population is smaller than
  Haiku's (32 vs 48 calls) because this run used tga at the
  [#140](https://github.com/bobmatnyc/trusty-git-analytics/pull/140) merge
  commit, which keeps merge commits out of the LLM tier entirely — the
  per-call token and cost figures compare directly, the raw call counts do
  not.
- **Sonnet 5.5 on Bedrock** (`us.anthropic.claude-sonnet-5-5`): $2.20 /
  $11.00 per million input / output tokens, US geo, AWS Bedrock pricing, read
  2026-10-06. Run cost $0.0681, which is $2.13 per 1,000 LLM-routed commits
  and $0.44 per 1,000 commits overall (20.7% routed share).
- **Jev (jev-1.13.0):** $0.042 per million input tokens. The output price is
  unconfirmed and assumed $0. The run used 25,718 input and 4,150 output
  tokens over 32 routed rows, for a run cost of $0.00108. That is $0.0338
  per 1,000 LLM-routed commits and $0.0070 per 1,000 commits overall
  (20.7% routed share). If TypeSafe charges for output tokens, the real
  figure is higher.

  ```
  cost per 1,000 routed commits = 1,000 × (input_tokens_per_call × input_price_per_token
                                            + output_tokens_per_call × output_price_per_token)
  ```

- The 26-week estimates scale the overall figure by the same factor as the
  Sonnet 5 row.

## Benefits

- **Haiku 4.5 over rules only:** weighted accuracy point estimate rises 13.0
  points (32.9% → 45.9%), and unanswered rows in the subsample fall from
  30/80 (37.5%) to 4/80 (5%) — a clear coverage gain. The two accuracy
  confidence intervals overlap ([20.3, 45.6] vs. [32.8, 58.9]), so the
  accuracy difference alone is not statistically clear at this sample size;
  the coverage gain does not depend on that overlap and holds regardless.
- **Sonnet 5 over Haiku 4.5:** weighted accuracy point estimate rises 1.2
  points (45.9% → 47.1%), well inside the overlap of the two confidence
  intervals ([32.8, 58.9] vs. [34.1, 60.1]) — not statistically clear at
  this sample size. Coverage is identical: both leave 4/80 rows unanswered.
  Sonnet 5 costs about 3x Haiku 4.5 per 1,000 LLM-routed commits ($2.01 vs.
  $0.68) for this non-clear gain.
- **Sonnet 5.5:** the same fine accuracy as Sonnet 5 (47.1%). Primary
  accuracy is 59.4% against 60.3%. Not distinguishable from Sonnet 5 or
  Haiku 4.5.
- **Jev:** fine accuracy 45.0% [32.0, 58.1], level with Haiku 4.5 (45.9%)
  and the Sonnet arms. Jev abstains on 11 of 32 routed rows and is correct
  on 17 of the 21 it answers (81%). It costs about 1/20 of Haiku 4.5 per
  1,000 routed commits ($0.0338 vs. $0.68) on the stated price basis.

## Recommendation

Four LLM options are measured: Bedrock Haiku 4.5, Sonnet 5, Sonnet 5.5 and
Jev. Their intervals overlap on every measure, so they are statistically
level at n = 76. All of them raise coverage over rules alone. Sonnet 5 and
5.5 cost about 3x Haiku 4.5 per 1,000 LLM-routed commits ($2.01 and $2.13
vs. $0.68) for no statistically clear gain. **Haiku 4.5 stays the default
LLM tier** (owner ruling).

Jev costs the least and is the most precise when it answers, but it
abstains more. It runs on real commit text by default, and with
`llm.jev.obfuscate: false` that text goes to TypeSafe. Set
`llm.jev.obfuscate: true` to pseudonymize it. Bedrock text stays in the
operator's AWS account. No option should run against the full commit history
without the owner's separate go-ahead. Every figure in this report comes from
the 100-commit subsample.
