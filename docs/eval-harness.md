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
`tga eval subsample --db` applies the same rule: it drops merge rows before
drawing, writes `is_merge: false` on every subset row, and refuses a row it
cannot resolve.

Stratum weights leave merges out too. An older `strata.json` counts merges
in each stratum population, and the window cannot be re-stratified without
re-running the rules, so the merge count is estimated from the sample: a
stratum with `m` merge rows among its `n` sample rows loses
`round(population · m / n)` from its population, and the window population
and `merges_excluded` move by the same total. Weighted accuracy, the
coverage curve and the abstention share all use the adjusted populations. A
sample drawn after this change holds no merges, so nothing is adjusted.
The estimate assumes merges are sampled in proportion, but the draw's
per-repo and per-author caps under-sample integrators who make most merges.
With `--db`, `report.md` therefore marks the populations "estimated" and
prints the exact merge count in the window (`window_start`..`window_end`,
every repository) beside the estimate; `report.json` carries both as
`window_merges_estimated` and `window_merges_exact`. Strata are not
recomputed, because they come from the rules in force at sampling time.

A label is correct when it equals the predicted category,
ignoring case; synonyms (`bug` vs `bugfix`) count as different, so raters
should use the vocabulary `tga eval sample` prints.

## Sampling design

1. **Window.** Commits from the last `--weeks` weeks (default 26), ending at
   the newest commit in the database rather than today, so a rerun on the
   same copy sees the same window.
2. **Re-classification.** The window is re-classified in memory with the
   config's rules (`build_rule_engine`, no LLM) and rule tracing on. Verdicts
   made outside the rule engine are taken from the stored `method` only when
   the config still reaches that tier: manual overrides always; LLM fallbacks
   when the config enables the LLM tier and the re-derived confidence is at
   or below `llm_fallback_threshold`; external-source verdicts when an
   external source is configured; repo fallbacks never (#111). The
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

## Re-scoring an existing sample under new rules

`tga eval score` scores the `predicted_category` stored in `sample.jsonl`,
which the rules of the draw produced. To score a labelled sample against a
different rules file, re-derive its predictions first (#111):

```bash
tga eval repredict --config ~/private/eval/config-v2.yaml \
    --sample ~/private/eval/sample.jsonl --db ~/private/eval/tga-copy.db \
    --out ~/private/eval/sample.v2.jsonl
tga eval score --config ~/private/eval/config-v2.yaml \
    --sample ~/private/eval/sample.v2.jsonl \
    --labels ~/private/eval/rater-a.csv --out ~/private/eval/report-v2
```

- **What changes.** Each row's `predicted_category`, `method`, `rule_id` and
  `confidence` become what the config's rules give for that commit. The
  commit is looked up by SHA and repository in `--db`, and classified from
  its message and merge flag, the inputs `tga classify` gives the cascade.
  No LLM or network tier is called. A stored verdict from a tier that is not
  re-run is carried only when the config's cascade would still reach that
  tier, the same rule `tga eval sample` applies: a manual override always;
  an LLM verdict only when the config enables the LLM tier and the
  re-derived confidence is at or below `llm_fallback_threshold`; an
  external-source verdict only when the config still enables an external
  source. A stored repo fallback is never carried, because `tga classify`
  never applies one. Otherwise the re-derived verdict replaces it.
- **Running the LLM on the sample only.** `repredict` never calls an LLM.
  To score the LLM tier, copy the database, list the sample SHAs one per
  line, and run `tga classify --force --shas <file>` against the copy with
  `llm_fallback_scope: unanswered`, then `tga eval sample`/`repredict` from
  that copy. `--shas` refuses an empty list or a SHA the database lacks.
  The run prints LLM call and token totals; per-call rows are in
  `llm_usage` (#111).
- **What stays.** The row set, the row order, each row's `stratum` and
  `weight`, and every commit field. Strata and weights describe the original
  draw, so `strata.json` still applies: write the output next to the source
  `sample.jsonl`, as above, or pass `--strata` to `score`.
- **Abstentions.** A row no tier matches becomes `uncategorized` with method
  `unclassified`, exactly as `tga eval sample` writes one. `score` treats it
  as it treats any such row: a prediction that a real label marks wrong.
- **Fail-closed.** A row whose commit is not in `--db` stops the run; no row
  is skipped or left with its old prediction. `--db` is opened read-only.
  `--config` must be passed explicitly. An existing `--out` or provenance
  file is never overwritten.
- **Provenance.** `sample.v2.provenance.json` records the tga version, the
  config file and each rules file with its BLAKE3 hash, the source sample
  and its hash, the database path, how many rows changed or abstain, the
  carried rows by method, and how many stored verdicts were superseded.

A relative `rules_file` in the config is resolved against the config
file's directory, as `database:` is, so `--config /abs/path/config.yaml`
works from any directory. `output.directory`, `cache.directory` and
`dora.datadog_dir` follow the same rule; use an absolute path to keep a
working-directory-relative one. Repository paths still resolve from the
working directory.

With `extend_defaults: false` the fuzzy tier is off, but the weighted-sum
tier still names its own categories (`feature`, `bugfix`, `chore`,
`integration`, `platform`, `docs`, `refactor`, `merge`). The only config
control over that tier today is `classification.weighted_sum.enabled:
false`, which turns it off; the commits it would have named then abstain.

For a worked example of this harness — a 400-commit sample, a 100-commit
rater subsample, and a cost-benefit analysis of the LLM tiers it scored —
see [`docs/classification/jev/`](classification/jev/README.md).
