# Classifier precision harness (`tga eval`)

> **Privacy.** Every file the harness writes — `sample.jsonl`, `labels.csv`,
> `strata.json`, `salt.txt`, `report.md`, `report.json` — contains commit
> text or is derived from it: subjects, bodies, changed paths, PR titles and
> ticket ids. Store the output directory privately, outside any repository,
> and delete it when the evaluation is done. The harness has no default
> output location; `--out` is required on every step, and it warns when the
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
- **Coverage at precision** — for each confidence value among the labelled
  rows, the weighted share of the population at or above it and the
  precision there. Use it to pick a confidence floor.
- **Confusion matrix** — predicted category × rater label.
- **Abstention share** — (catch_all + uncategorized/Unknown population) ÷
  window population, from `strata.json`.
- **Cohen's kappa** — agreement between two raters on the commits both
  labelled, corrected for chance, with that overlap reported as `n`. The two
  sheets may cover different rows.

Labels `unclear`, `mixed` and `release_merge` are counted and reported per
label but score as no answer, left out of precision (#111).

A commit with 2+ parents is a merge, and it is excluded from metrics and
from the eval. Squash and rebase commits (1 parent) are normal commits,
classified by content. `tga eval sample` never draws a merge and writes
`is_merge` on every row. `tga eval score` drops merge rows with their labels
and reports how many; for a sample written before this rule, pass `--db`
with a copy of the database so each row's merge flag is resolved by SHA. A
row whose status cannot be resolved stops the score with an error.

A label is correct when it equals the predicted category,
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
   stratum sample size. The scorer does not use that stored value: it weights
   each labelled row by stratum population ÷ rows labelled in that stratum,
   so a subset or a partly filled sheet is weighted by the rows it actually
   has.

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
   off the order. Raters fill `label` with one category name, `unclear`,
   `mixed` or `release_merge`, and may use `note`. A row left blank counts as unlabelled, not as
   an error, so a sheet can be scored while it is only partly filled. Two
   raters give a kappa; disagreements can be settled in an adjudication file
   with the same columns.

3. **Subsample (optional).** When one rater labels the whole sample and
   another only part of it, draw that part as a subset:

   ```bash
   tga eval subsample --from ~/private/eval/sample.jsonl --size 100 \
       --seed 20260924 --out ~/private/eval/rater1-100
   ```

   Each stratum gets a share of `--size` proportional to its share of the
   source rows, by largest remainder, so the counts add up to exactly
   `--size`: 212/85/37/66 of 400 becomes 53/21/9/17 of 100. Within a stratum
   the rows are sorted by SHA and shuffled with a stream derived from
   `--seed`, so the same seed on the same sample draws the same subset. No
   label file is read. The output is the subset's `sample.jsonl` (each row's
   `weight` rescaled to population ÷ subset rows), a `labels.csv` with the
   same columns, redaction and salted-hash row order as `tga eval sample`
   writes, and a `strata.json` whose `sampled` counts are the subset's and
   whose `subsample` block records the seed and sizes. On Unix the created
   directory is mode 0700 and the files 0600. The command refuses to
   overwrite an existing `sample.jsonl`, `labels.csv` or `strata.json`, so a
   rerun cannot erase a sheet being filled in.

4. **Score.**

   ```bash
   tga eval score --sample ~/private/eval/sample.jsonl \
       --labels ~/private/eval/rater-a.csv --labels ~/private/eval/rater-b.csv \
       --adjudicated ~/private/eval/adjudicated.csv --out ~/private/eval/report
   ```

   **The first `--labels` file is the scored rater.** Precision, weighted
   accuracy and coverage use its labels over the rows it labelled; the
   adjudication file replaces its label on the rows it names, and naming a
   row the first rater left blank is an error. The second file only feeds
   Cohen's kappa, over the SHAs both files labelled, and may cover different
   rows. A disagreement no adjudication settles is counted as unresolved,
   but the row is still scored with the first rater's label.

   To score a subset rater against a full-sample rater, pass the subset's
   sheet first and the source sample, whose rows hold both raters' SHAs:

   ```bash
   tga eval score --sample ~/private/eval/sample.jsonl \
       --labels ~/private/eval/rater1-100/labels.csv \
       --labels ~/private/eval/rater2-full.csv --out ~/private/eval/report-100
   ```

   Kappa then reports `n = 100` and precision covers those 100 rows, each
   weighted by its stratum population ÷ labelled rows in the stratum.

   Valid labels come from `--config` when it is passed, otherwise from the
   categories recorded in `strata.json`; the sample's predicted categories
   are always valid. An unknown label, or a label for a SHA outside
   `--sample`, stops the run with the offending SHA. Output: `report.md` and
   `report.json`.
