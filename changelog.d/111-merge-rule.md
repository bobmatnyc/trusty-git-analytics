Changed

- A commit with 2+ parents is a merge, and it is excluded from metrics and
  from the eval (#111). Squash and rebase commits (1 parent) are normal
  commits, classified by content. Reports drop merges from total and
  per-author, per-repo and weekly commit counts, the category breakdown,
  `fact_weekly_engineer` and `fact_weekly_quality`, DORA change-failure rate
  and MTTR, the quality score, `tga author` drill-downs and period trends.
  Merges stay in the database, and a deploy whose `git_sha` is a merge still
  gets a measured lead time. Historical totals drop by the merge count.
- `tga eval sample` never draws a merge; `strata.json` gains
  `merges_excluded`, and `population` no longer counts merges (#111).
- `tga eval score` excludes merge rows and their labels and reports
  `merges_excluded`. A sample written before this change needs `--db` to
  resolve each row's merge flag by SHA; a row it cannot resolve is an error,
  never scored as a non-merge (#111).
