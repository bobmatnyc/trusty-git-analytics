Added

- `tga eval repredict --config <cfg> --sample <sample.jsonl> --db <tga.db>
  --out <out.jsonl>` re-derives an existing sample's predictions under a
  config's rules, so a sample labelled under old rules can be scored under
  new ones (#111). Rows keep their order, SHA, stratum and weight; only
  `predicted_category`, `method`, `rule_id` and `confidence` change. A row
  whose commit is not in the database stops the run, the database is opened
  read-only, and `<out>.provenance.json` records the tga version and the
  BLAKE3 hashes of the config and its rules files.
