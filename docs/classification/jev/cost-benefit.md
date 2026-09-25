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
| Rules + Bedrock Claude Sonnet 5 | NOT YET MEASURED | assumed same 20.7% routing (not re-measured) | $1.36 — **projected**, not measured | $0.28 — **projected** | ≈ $12.82 — **projected** | AWS Bedrock, in the operator's AWS account and region: AWS processes the text under the operator's AWS agreement; per Bedrock's data terms, the model provider does not receive it | Same as Haiku, on tga 9.0.1 or later: earlier versions send a `temperature` parameter Sonnet 5 rejects; 9.0.1 no longer sends it on Bedrock. A Sonnet 5 re-measurement is pending |
| Rules + Jev (TypeSafe's hosted model) | NOT YET MEASURED | not measured | formula only, see below | not computable yet | not computable yet | TypeSafe's service sees routed commit text over the network — a new third party this pipeline does not use today | A Jev API key (read from the env var named by `api_key_env`), plus the `llm.source: jev` wiring, which does not exist yet — see README §5 |

## Price basis

- **Haiku 4.5 on Bedrock:** list price $1 / $5 per million input / output
  tokens. Bedrock in `us-east-1` normally matches the list price; the AWS
  bill is the authoritative source, not this document. The measured run
  used 48 LLM calls, 24,171 input tokens and 1,736 output tokens (about 504
  input and 36 output tokens per call).
- **Sonnet 5:** list price $2 / $10 per million input / output tokens —
  exactly double Haiku's. The $1.36 / $0.28 / $12.82 figures above are
  Haiku's measured token counts re-priced at Sonnet 5's list price; they
  carry no accuracy signal, only a cost signal, because Sonnet 5 classifies
  commits differently and has not been run.
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
- **Sonnet 5 and Jev:** no accuracy or coverage benefit can be stated yet —
  neither has been run against the subsample.

## Recommendation

The only option with a measured accuracy and cost is rules + Bedrock Haiku
4.5: cheap (≈$6.41 projected for the full 26-week window) and a clear
coverage improvement, with an accuracy gain whose point estimate is large
but whose confidence interval still overlaps the rules-only baseline.
Sonnet 5 and Jev both need their own measurement — the same
`tga eval sample` / `subsample` / `repredict` / `score` workflow used above
— before either can be chosen over Haiku 4.5 on cost or accuracy grounds.
Neither should run against the full commit history without the owner's
separate go-ahead; every figure in this report comes from the 100-commit
subsample, not a full-history run.
