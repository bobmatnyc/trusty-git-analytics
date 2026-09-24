Added

- `tga eval score` accepts the label `release_merge` (eval scheme v2), which
  scores as no answer like `unclear` and `mixed`; `report.json` and
  `report.md` count it on its own (#111).
- `tga eval score --config` accepts every category the config's rules file
  emits, not only those in its taxonomy, so a v2 rules file is enough to
  validate v2 labels (#111).
- `tga eval score --db <path>` resolves the merge flag of sample rows that
  lack one, from a read-only copy of the tga database (#111).
