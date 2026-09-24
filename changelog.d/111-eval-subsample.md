Added

- `tga eval subsample --from <sample.jsonl> --size N --seed S --out <dir>`
  draws a seeded subset of an existing eval sample for a second rater
  (#111). Each stratum's share of N follows its share of the source rows,
  by largest remainder, so the subset has exactly N rows; no label file is
  read. It writes the subset's `sample.jsonl`, a blind `labels.csv` with the
  same columns and redaction as `tga eval sample`, and a `strata.json` that
  records the seed. The directory is created 0700 and the files 0600 on
  Unix, and existing output files are never overwritten.
