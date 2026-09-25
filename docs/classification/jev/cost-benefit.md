# Cost-benefit analysis: rules vs. LLM tiers ([#111](https://github.com/bobmatnyc/trusty-git-analytics/issues/111))

Figures below come from the 100-commit subsample in
[`README.md`](README.md) §3. "Routed" means a commit the rules left
unanswered and that was sent to the LLM tier; "overall" scales that per-call
cost by the share of non-merge commits actually routed.

## Options compared

| Option | Accuracy (weighted, 95% CI) | Unanswered share | Cost / 1,000 LLM-routed commits | Cost / 1,000 commits overall | 26-week window estimate | Who sees commit text | Operational needs |
|---|---|---|---|---|---|---|---|
| Rules only (v2 + precision rules) | 32.9% [20.3, 45.6] — measured | 30/80 (37.5%) of the labelled subsample; 51.4% of all stored commits get a rule-based category, so ~48.6% do not | none — no LLM call | $0 | $0 | No third party; classification runs locally against the operator's own database | None beyond the rules file already in the config |
| Rules + Bedrock Claude Haiku 4.5 | **45.9% [32.8, 58.9] — measured** | 4/80 (5%) of the subsample; 20.7% of non-merge commits are routed to the LLM under the current rules | $0.68 | $0.14 | ≈ $6.41 | AWS Bedrock, in the operator's AWS account and region: AWS processes the text under the operator's AWS agreement; per Bedrock's data terms, the model provider does not receive it | `cargo build --features bedrock`; AWS credentials via the default chain or `AWS_PROFILE`; Bedrock model access enabled for the account's region |
| Rules + Bedrock Claude Sonnet 5 | **47.1% [34.1, 60.1] — measured** | 4/80 (5%) of the subsample, same as Haiku 4.5; the overall and 26-week figures reuse the documented 20.7% routed share, not independently re-measured for Sonnet 5 | $2.01 | $0.42 | ≈ $18.92 | AWS Bedrock, in the operator's AWS account and region: AWS processes the text under the operator's AWS agreement; per Bedrock's data terms, the model provider does not receive it | Same as Haiku; on Bedrock, needs tga at or after the [#140](https://github.com/bobmatnyc/trusty-git-analytics/pull/140) merge commit, which drops `temperature` from every Bedrock request — an earlier version sends it and Sonnet 5 rejects the call. On OpenRouter, Sonnet 5 accepts `temperature: 0.0` directly (verified 2026-09-25); only Bedrock needed the fix |
| Rules + Jev (TypeSafe's hosted model) | NOT YET MEASURED | not measured | formula only, see below | not computable yet | not computable yet | TypeSafe's service sees routed commit text over the network — a new third party this pipeline does not use today | A Jev API key (read from the env var named by `api_key_env`), plus the `llm.source: jev` wiring, which does not exist yet — see README §5 |

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
- **Jev:** cost formula, not a number, because Jev's own token profile is
  unmeasured:

  ```
  cost per 1,000 routed commits = 1,000 × (input_tokens_per_call × input_price_per_token
                                            + output_tokens_per_call × output_price_per_token)
  ```

  As an illustration only — **not** a measurement — plugging in Haiku's
  measured ~504 input tokens/call against an input price of **$0.042 per
  million tokens (TypeSafe list price, confirmed by the owner 2026-09-24)**
  gives an input-only cost of ≈ $0.021 per 1,000 routed commits. No output
  price is available, so this is a partial number: the real total needs
  Jev's own token counts (its prompt and response shape likely differ from
  Haiku's) and its output price.

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
- **Jev:** no accuracy or coverage benefit can be stated yet — it has not
  been run against the subsample.

## Recommendation

Two options now have a measured accuracy and cost: rules + Bedrock Haiku 4.5
and rules + Bedrock Sonnet 5. Both give the same coverage gain over rules
alone (4/80 unanswered vs. 30/80), and their accuracy point estimates each
overlap the other's confidence interval as well as the rules-only baseline's.
Sonnet 5 costs about 3x Haiku 4.5 per 1,000 LLM-routed commits ($2.01 vs.
$0.68; ≈$18.92 vs. ≈$6.41 over the 26-week window) for no statistically clear
accuracy gain on this sample, so **Haiku 4.5 remains the default
recommendation**. Jev still needs its own measurement — the same
`tga eval sample` / `subsample` / `repredict` / `score` workflow used above
— before it can be compared on cost or accuracy grounds. Neither Sonnet 5
nor Jev should run against the full commit history without the owner's
separate go-ahead; every figure in this report comes from the 100-commit
subsample, not a full-history run.
