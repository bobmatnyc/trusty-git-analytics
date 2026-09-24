Changed

- A commit with 2+ parents is a merge, and it is excluded from metrics and
  from the eval (#111). Squash and rebase commits (1 parent) are normal
  commits, classified by content. Reports drop merges from total and
  per-author, per-repo and weekly commit counts, the category breakdown,
  the unresolved-author commit count, `fact_weekly_engineer` and
  `fact_weekly_quality`, the quality score, `tga author` drill-downs and
  period trends. DORA change-failure rate and MTTR skip merges in both
  places they are computed: the report aggregator, and `tga dora`, whose
  failure scan now checks the first non-merge commit after a deploy. Merges
  stay in the database, and a deploy whose `git_sha` is a merge still gets a
  measured lead time. Historical totals drop by the merge count.
- `fact_weekly_engineer` and `fact_weekly_quality` rows are written with
  `formula_version` `v2` (#111). Each persist run deletes rows of an older
  formula and, for every author it writes, rows it no longer produces, so a
  merge-only week keeps no merge-inclusive row.
- `tga eval sample` never draws a merge; `strata.json` gains
  `merges_excluded`, and `population` no longer counts merges (#111).
- `tga eval score` excludes merge rows and their labels and reports
  `merges_excluded`. A sample written before this change needs `--db` to
  resolve each row's merge flag by SHA; a row it cannot resolve is an error,
  never scored as a non-merge (#111). Each stratum's population loses its
  estimated merge share (population · merge rows ÷ sample rows), so weighted
  accuracy, coverage and abstention no longer weigh merges. `report.md`
  marks those populations estimated and, with `--db`, prints the window's
  exact merge count beside the estimate (`window_merges_estimated`,
  `window_merges_exact` in `report.json`).
- `tga eval subsample` drops merge rows before drawing and refuses rows of
  unknown merge status; `--db` resolves rows that carry no merge flag (#111).
