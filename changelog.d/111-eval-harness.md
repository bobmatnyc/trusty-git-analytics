Added

- `tga eval sample` and `tga eval score`, a classifier precision harness
  (#111). `sample` re-classifies a window of a read-only database copy with
  rule tracing and draws a seeded, stratified sample capped per repository
  and per author, writing `sample.jsonl`, a rater sheet `labels.csv` with the
  prediction hidden, and `strata.json`. `score` turns one or two rater files
  (plus optional adjudication) into `report.md` / `report.json`: precision
  per rule, method and stratum with Wilson 95% intervals, stratum-weighted
  accuracy, a coverage-at-precision curve, a confusion matrix, the
  abstention share and Cohen's kappa. The outputs contain commit text and
  have no default location; see `docs/eval-harness.md`.
