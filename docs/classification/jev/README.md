# Classification accuracy report and Jev option ([#111](https://github.com/bobmatnyc/trusty-git-analytics/issues/111))

> **Privacy.** This report holds aggregates only: no commit text, SHAs, repo
> names, author names or emails, and nothing from the operator's private
> data or from any private repository. Where the operator's
> organisation needs a mention, it is "the operator's organisation," never a
> name. Routed commit text goes to the LLM tier you configure.
> **Bedrock:** AWS processes it in the operator's AWS account under the
> operator's AWS agreement; per Bedrock's data terms the model provider does
> not receive it. **Jev:** the default is real commit text, as stored, names
> included, and with `llm.jev.obfuscate: false` (the default) it goes to
> TypeSafe. Set `llm.jev.obfuscate: true` to pseudonymize names, trailers,
> repos, paths, tickets, URLs and hashes first (owner ruling 2026-10-06; see
> [`configuration.md`](../../requirements/configuration.md)). See
> [`cost-benefit.md`](cost-benefit.md) for the per-option detail.

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
report measures rules, rules + Bedrock Haiku 4.5, Sonnet 5 and Sonnet 5.5,
and rules + Jev (jev-1.13.0). Haiku 4.5 stays the default LLM tier.

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
| Rules + Bedrock Claude Sonnet 5.5 (`us.anthropic.claude-sonnet-5-5`) | **47.1% [34.1, 60.1]** | LLM-only precision 20/26 (77%) |
| Rules + Jev (jev-1.13.0, TypeSafe's hosted model, real commit text) | **45.0% [32.0, 58.1]** | 32 LLM-routed rows: 21 adopted, 11 abstained, 0 failed; LLM-only precision 17/21 (81%); 25,718 input / 4,150 output tokens |

All five runs use the same 100-row sample, the same rater-1 labels and the
same scorer: `tga eval score` with buckets. Rater 2 is the second rater.
76 rows are scored. Accuracy is weighted by stratum, with 95% confidence
intervals. The three accuracy columns are:

- **Fine:** the exact category, as in the table above.
- **Primary (bucket):** the bucket the category maps to.
- **Secondary:** fine-category accuracy within buckets that have more than
  one category. Internal Tooling is excluded.

Buckets: Maintenance = `bug_fix`, `devops`, `security`, `qa`, `upkeep`.
Value Creation = `new_feature`, `integration`, `content_design`.
Foundational Investment = `platform_infrastructure`, `data_science`.
Internal Tooling = `internal_tooling`. The map is config-driven. Consumers
supply it through `classification.buckets` or a rules-file `buckets:`.

| Run | Fine | Primary (bucket) | Secondary |
|---|---|---|---|
| Rules | 32.9 [20.3, 45.6] | 44.4 [31.9, 56.9] | 35.5 [21.7, 49.2] |
| Bedrock Haiku 4.5 | 45.9 [32.8, 58.9] | 59.9 [47.1, 72.7] | 48.9 [34.7, 63.0] |
| Bedrock Sonnet 5 | 47.1 [34.1, 60.1] | 60.3 [47.5, 73.0] | 50.2 [36.1, 64.3] |
| Bedrock Sonnet 5.5 | 47.1 [34.1, 60.1] | 59.4 [46.6, 72.3] | 50.2 [36.1, 64.3] |
| Jev jev-1.13.0, real text | 45.0 [32.0, 58.1] | 57.4 [44.5, 70.2] | 48.0 [33.9, 62.2] |

The confidence intervals of the four LLM arms overlap on every measure. At
n = 76 the four LLM arms are statistically level. The gain of each LLM arm
over rules alone is also not statistically clear on fine accuracy. Sonnet 5
costs roughly 3x Haiku 4.5 per LLM call, and Jev costs far less than either
(see [`cost-benefit.md`](cost-benefit.md)). Jev abstains more (11 of 32
routed rows) but is the most precise when it answers: 17/21 (81%), against
20/26 (77%) for Sonnet 5.5 and 19/26 (73%) for Haiku 4.5. No arm is directly
comparable to the rules row on precision per answered row, because the LLM
tier only sees the rows the rules left unanswered, a harder subset.

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
  source: bedrock                      # bedrock | openrouter | anthropic-api | jev
  region: us-east-1                    # Bedrock only; falls back to the AWS SDK's own resolution
  model: us.anthropic.claude-haiku-4-5-20251001-v1:0
  api_key_env: MY_LLM_API_KEY          # openrouter / anthropic-api only; ignored for bedrock
```

- **Bedrock:** the default build includes it; no build flag is needed
  (only a `--no-default-features` build leaves it out). Current Claude models on Bedrock need a `us.` or `global.`
  inference-profile model id, not the bare model id. Credentials come from
  the AWS default credential chain (env vars, `~/.aws/credentials` profile,
  SSO, instance role) — set `AWS_PROFILE` to pick a non-default profile. No
  secret is stored in the config file.
- **OpenRouter / Anthropic API:** `api_key_env` names the environment
  variable holding the key; the key itself is never written to the config.

### Jev

`llm.source: jev` is wired (#111) as a fourth `llm.source` value. Like
`openrouter` and `anthropic-api`, it reads its API key from the environment
variable named by `api_key_env` (`TYPESAFE_API_KEY` by default). Jev runs on
real commit text by default, and with obfuscation off the text goes to
TypeSafe. Set `llm.jev.obfuscate: true` to pseudonymize it first (owner
ruling 2026-10-06).

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
