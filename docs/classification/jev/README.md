# Classification accuracy report and Jev option ([#111](https://github.com/bobmatnyc/trusty-git-analytics/issues/111))

> **Privacy.** This report holds aggregates only: no commit text, SHAs, repo
> names, author names or emails, and nothing from the operator's private
> data or from any private repository. Where the operator's
> organisation needs a mention, it is "the operator's organisation," never a
> name. Separately, for the Bedrock options measured or priced below: AWS
> processes routed commit text under the operator's AWS agreement, and per
> Bedrock's data terms the model provider does not receive it (see
> [`cost-benefit.md`](cost-benefit.md) for the per-option detail).

## 1. Purpose and scope

[#111](https://github.com/bobmatnyc/trusty-git-analytics/issues/111) asks how
accurate `tga classify`'s rule cascade is against a human's judgment, and
whether an LLM fallback tier is worth adding to raise it. This report
measures that accuracy with the `tga eval` harness (see
[`docs/eval-harness.md`](../../eval-harness.md)) and lays out what each
option to improve it costs. It does not choose an option — see
[`cost-benefit.md`](cost-benefit.md) §5 for what evidence is still missing
and what the operator needs to decide.

"Jev" is TypeSafe's hosted commit-classification model, proposed as a fourth
`llm.source` alongside `openrouter`, `bedrock` and `anthropic-api`. This
report measures the options that exist today (rules, rules + Bedrock Haiku
4.5, rules + Bedrock Sonnet 5) and prices the one that does not yet have a
measurement (Jev). Adding Jev as a wired `llm.source` is a separate
engineering task; §5 below describes it as planned only.

## 2. Method

- **Sample.** `tga eval sample --seed 20260923 --size 400`, drawn from a
  26-week window of the operator's commit history, stratified by cascade
  tier with per-repo/per-author caps (see
  [`docs/eval-harness.md`](../../eval-harness.md) §"Sampling design").
- **Subsample.** `tga eval subsample --seed 20260924 --size 100`, drawn from
  the 400-commit sample, proportional to each stratum's share.
- **Raters.** Two raters labelled the 100-commit subsample under a written
  scheme (scheme v2, below). Rater 1 is the project owner. **Rater 2 is not
  a human** — it is Claude Opus 5.5, run blind under the same written rules
  and given no more information than the human rater had. Its labels are an
  inter-rater-agreement check, not a second human opinion.
- **Scheme v2 categories.** `security`, `devops`, `qa`, `bug_fix`,
  `new_feature`, `internal_tooling`, `integration`,
  `platform_infrastructure`, `upkeep`, `data_science`, plus the rater-only
  labels `unclear`, `mixed` and `release_merge`. The three rater-only labels
  score as no-answer, not as a wrong prediction (see
  [`docs/requirements/classification.md`](../../requirements/classification.md)
  §"Eval scheme v2").
- **Merge rule.** A commit with 2+ parents is a merge and is excluded from
  metrics and from this eval entirely. Squash and rebase commits (one
  parent) are ordinary commits, classified by content.
- **Tools.** `tga eval sample`, `tga eval subsample`, `tga eval repredict`
  and `tga eval score`
  ([#130](https://github.com/bobmatnyc/trusty-git-analytics/issues/130),
  [#132](https://github.com/bobmatnyc/trusty-git-analytics/issues/132)).
  Weighted accuracy is stratum-weighted, with merges excluded from the
  window population; the exact window merge count came from `--db` against
  a copy of the operator's database.

## 3. Results

- **Sample:** 100 commits drawn for labelling; 20 were merges and excluded;
  80 labelled; 76 scored (2 labelled `unclear` and 2 `release_merge`, both
  no-answer).
- **Rater agreement:** Cohen's kappa 0.915 (observed agreement 92.5%,
  expected-by-chance 12.0%, n = 80).

Tool weighted accuracy against rater 1, with 95% confidence intervals:

| Configuration | Weighted accuracy (95% CI) | Notes |
|---|---|---|
| Old scheme, predictions frozen in the sample | 23.2% [11.4, 34.9] | Baseline; scored under a different scheme, not comparable to the rows below |
| v2 rules, weighted-sum tier **on** | 31.0% [18.5, 43.6] | Weighted-sum tier was right on 0/9 of its own rows and emitted names outside the scheme |
| v2 rules, weighted-sum tier **off** | 31.0% [18.5, 43.6] | Same result with the tier disabled ([#131](https://github.com/bobmatnyc/trusty-git-analytics/issues/131)); the tier is now off by default |
| v2 rules + new precision rules (`data_science`, `internal_tooling`, `upkeep`) | 32.9% [20.3, 45.6] | Rules built without seeing the 100 labelled commits; each spot-checked ≥17/20 correct on other commits. 30/80 rows left unanswered; precision on answered rows ≈41% (19/46) |
| Rules + LLM tier on unanswered rows only, Bedrock Claude Haiku 4.5 | **45.9% [32.8, 58.9]** | 4/80 rows still unanswered; LLM precision on its own rows 73% (19/26); 48 LLM calls, 24,171 input / 1,736 output tokens |
| Rules + LLM tier on unanswered rows only, Bedrock Claude Sonnet 5 | **47.1% [34.1, 60.1]** | 4/80 rows still unanswered; 32 LLM calls (28 adopted, 4 abstained, 0 out-of-set, 0 failed), 23,321 input / 1,197 output tokens |
| Rules + Jev (TypeSafe's hosted decision model) | NOT YET MEASURED | Needs an API key and the owner's go-ahead |

The 45.9% and 47.1% rows are the two measured LLM configurations. Both leave
the same 4/80 rows unanswered, and both confidence intervals overlap each
other's as well as the 32.9% rules-only row's — so neither the Haiku-over-rules
gain nor the Sonnet-over-Haiku gain is statistically clear at this sample
size. Sonnet 5 costs roughly 3x Haiku 4.5 per LLM call (see
[`cost-benefit.md`](cost-benefit.md)) for no statistically clear accuracy
gain on this sample, so Haiku 4.5 remains the default recommendation.
Neither is directly comparable to the 32.9% row on precision-per-answered-row,
because the LLM tier only sees the 30 rows the rules left unanswered — a
harder subset than the full 80.

**Method note.** The Sonnet 5 run used tga at the
[#140](https://github.com/bobmatnyc/trusty-git-analytics/pull/140) merge
commit, which also keeps merge commits out of the LLM tier entirely. Its
LLM-eligible population is therefore slightly smaller than the Haiku run's
(32 vs 48 calls), so the per-call figures in `cost-benefit.md` compare more
directly than the raw call counts.

**Whole-history coverage.** Reclassifying all commits stored in the
operator's database with the v2 rules (no LLM) covers 51.4% of them — i.e.
just over half of all stored commits get a rule-based, non-catch-all
category with no LLM call.

## 4. Cost-benefit analysis

See [`cost-benefit.md`](cost-benefit.md) for the full table, the Jev cost
formula, and the recommendation.

## 5. How to enable the LLM tier

Two config surfaces control the LLM fallback tier; see
[`docs/requirements/configuration.md`](../../requirements/configuration.md)
§"llm" and §"classification" for the authoritative field list, and
`src/core/config/llm.rs` / `src/core/config/mod.rs` on `origin/main` for the
exact struct fields — this file summarizes, the source is the contract.

```yaml
classification:
  use_llm: true                        # or omit: an `llm:` section below enables it automatically
  llm_fallback_scope: unanswered       # send only rule-unanswered commits to the LLM, not low-confidence hits

llm:
  source: bedrock                      # bedrock | openrouter | anthropic-api
  region: us-east-1                    # Bedrock only; falls back to the AWS SDK's own resolution
  model: us.anthropic.claude-haiku-4-5-20251001-v1:0
  api_key_env: MY_LLM_API_KEY          # openrouter / anthropic-api only; ignored for bedrock
```

- **Bedrock:** build the binary with `cargo build --release --features
  bedrock`. Current Claude models on Bedrock need a `us.` or `global.`
  inference-profile model id, not the bare model id. Credentials come from
  the AWS default credential chain (env vars, `~/.aws/credentials` profile,
  SSO, instance role) — set `AWS_PROFILE` to pick a non-default profile. No
  secret is stored in the config file.
- **OpenRouter / Anthropic API:** `api_key_env` names the environment
  variable holding the key; the key itself is never written to the config.

### Jev (planned)

Jev is not wired into `tga` yet. The plan is a fourth `llm.source: jev`
value that, like `openrouter` and `anthropic-api`, reads its API key from
the environment variable named by `api_key_env` — no change to the shape of
the `llm:` section. An engineer implements this separately; this report
only measures the cost of doing so (§4) once that key exists.

## 6. Limits

- **Sample size.** 80-100 labelled, non-merge commits yield wide confidence
  intervals (roughly ±12-13 points); a difference smaller than that between
  two rows in §3 is not distinguishable from noise.
- **One human rater.** Rater agreement (kappa 0.915) is measured against a
  single owner, not a panel; it says the two raters agree with each other,
  not that either is "correct" in some external sense.
- **Rater 2 is an LLM,** not a second human. Its blind agreement with the
  owner is evidence the written scheme is unambiguous enough for two
  independent judges to apply consistently, not a second independent human
  opinion.
- **Stratification.** The sample is stratified by cascade tier with per-repo
  and per-author caps, so it is not a uniform random sample of commits; the
  weighting in §3 corrects for stratum size but not for the caps.
- **No full-history LLM run yet.** Every LLM figure above comes from the
  100-commit subsample. Nothing has been measured, or should run, against
  the full window or full history without the owner's separate go-ahead
  (see [`cost-benefit.md`](cost-benefit.md) §5).
- **Sonnet 5's LLM-eligible population is smaller than Haiku's.** The Sonnet
  5 run excludes merge commits from the LLM tier entirely (32 calls vs
  Haiku's 48); see the method note in §3. The per-call token and cost
  figures still compare directly.
