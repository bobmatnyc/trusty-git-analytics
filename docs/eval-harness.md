# Classifier precision harness (`tga eval`)

> **Privacy.** Every file the harness writes — `sample.jsonl`, `labels.csv`,
> `strata.json`, `salt.txt`, `report.md`, `report.json` — contains commit
> text or is derived from it: subjects, bodies, changed paths, PR titles and
> ticket ids. Store the output directory privately, outside any repository,
> and delete it when the evaluation is done. The harness has no default
> output location; `--out` is required on both steps, and it warns when the
> directory sits inside a git work tree. The rater sheet `labels.csv` drops
> identity trailers (`Co-authored-by:`, `Signed-off-by:`, `Reviewed-by:` and
> similar) and replaces e-mail addresses with `<email>`; `sample.jsonl` keeps
> the full commit text.

## What it measures

`tga classify` gives every commit a category, a confidence and a method. The
harness measures how often that category is right, as judged by people:

- **Precision per rule** — for each rule id (`builtin#<id>`, `<file>#<id>`,
  `catch_all`, `weighted_sum:<cat>/<signal>`, `fuzzy:<heuristic>`,
  `jira_project:<KEY>`, `manual_override`, `external_source`, `llm`,
  `unclassified`), the share of labelled commits whose label equals the
  predicted category, with a Wilson 95% interval and n.
- **Precision per method** (the cascade tier) and **per stratum**.
- **Stratum-weighted accuracy** — Σ W_h · p_h, where W_h is the stratum's
  share of the population, with a normal-approximation 95% interval. It
  estimates accuracy over the whole window, not over the sample.
- **Coverage at precision** — for each confidence value in the sample, the
  weighted share of the population at or above it and the precision there.
  Use it to pick a confidence floor.
- **Confusion matrix** — predicted category × rater label.
- **Abstention share** — (catch_all + uncategorized/Unknown population) ÷
  window population, from `strata.json`.
- **Cohen's kappa** — agreement between two raters on the commits both
  labelled, corrected for chance.

Labels `unclear` and `mixed` are counted and reported but left out of
precision. A label is correct when it equals the predicted category,
ignoring case; synonyms (`bug` vs `bugfix`) count as different, so raters
should use the vocabulary `tga eval sample` prints.

## Sampling design

1. **Window.** Commits from the last `--weeks` weeks (default 26), ending at
   the newest commit in the database rather than today, so a rerun on the
   same copy sees the same window.
2. **Re-classification.** The window is re-classified in memory with the
   config's rules (`build_rule_engine`, no LLM) and rule tracing on. Verdicts
   made outside the rule engine — manual overrides, external ticket sources,
   LLM fallbacks, repo fallbacks — are taken from the stored `method`. The
   database is opened read-only; the harness refuses a writable handle.
3. **Strata.** `exact`; `regex_high` (regex, confidence ≥ 0.9); `regex_mid`
   (regex, 0.55–0.7); `regex_other` (other regex bands); `weighted_sum`;
   `fuzzy`; `catch_all`; `unknown` (no match, or category
   `uncategorized`/Unknown); `other` (verdicts decided outside the engine).
4. **Allocation.** `--size` (default 400) is split equally across non-empty
   strata. A stratum with fewer eligible commits than its share gives all it
   has and passes the rest to the others.
5. **Caps.** Within each stratum at most `--cap` (default 5) commits per
   repository and per author, so no single team dominates a stratum. A
   database with one repository therefore yields at most `--cap` commits per
   stratum; raise `--cap` there.
6. **Seed.** `--seed` is required. Each stratum is shuffled with a
   seed-derived stream after sorting by SHA, so the same seed on the same
   database draws the same sample, and a different seed draws another.
7. **Weights.** Each sampled commit carries weight = stratum population ÷
   stratum sample size; the scorer uses the stratum populations to weight.

Author e-mails never leave the database: `author_hash` is a salted BLAKE3
hash. Without `--salt` a salt is generated and saved as `salt.txt`.

### Why `weighted_sum` and `fuzzy` can be empty

The cascade runs exact → regex → weighted sum → fuzzy. With the built-in
rules, the lowest-priority regex rule is a catch-all that matches any
non-empty message, so it answers before the weighted-sum and fuzzy tiers are
reached. Those strata only fill when a custom rules file drops the catch-all
(`extend_defaults: false` without one) or for messages the catch-all does not
match. An empty stratum is reported with population 0 and its share of the
sample goes to the others.

## Steps

1. **Sample.** Work on a copy of the database:

   ```bash
   cp tga.db ~/private/eval/tga-copy.db
   tga eval sample --config config.yaml --db ~/private/eval/tga-copy.db \
       --seed 20260923 --out ~/private/eval
   ```

   Output: `sample.jsonl` (one commit per line: sha, repo, date, author_hash,
   subject, body, paths, diffstat, PR title, ticket id, issue type, stratum,
   method, rule_id, predicted category, confidence, weight), `labels.csv`,
   `strata.json`, and `salt.txt` when the salt was generated. The command
   prints the per-stratum population and sample counts and the valid labels.

2. **Label.** Give each rater a copy of `labels.csv`. It hides the predicted
   category and orders rows by a salted hash, so the stratum cannot be read
   off the order. Raters fill `label` with one category name, `unclear` or
   `mixed`, and may use `note`. Two raters give a kappa; disagreements can be
   settled in an adjudication file with the same columns.

3. **Score.**

   ```bash
   tga eval score --sample ~/private/eval/sample.jsonl \
       --labels ~/private/eval/rater-a.csv --labels ~/private/eval/rater-b.csv \
       --adjudicated ~/private/eval/adjudicated.csv --out ~/private/eval/report
   ```

   The final label of a commit is the adjudicated one, else the single
   rater's, else the label both raters agree on; an unsettled disagreement
   is not scored and is counted. Valid labels come from `--config` when it is
   passed, otherwise from the categories recorded in `strata.json`; the
   sample's predicted categories are always valid. An unknown label stops the
   run with the offending SHA. Output: `report.md` and `report.json`.
