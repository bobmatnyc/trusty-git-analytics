Changed

- `tga eval score` scores the first `--labels` file only (#111). Precision,
  weighted accuracy and coverage use that rater's labels over the rows it
  labelled, with `--adjudicated` overriding them; the second file feeds
  Cohen's kappa over the SHAs both labelled, and the two sheets may cover
  different rows. An unadjudicated disagreement is still counted but no
  longer drops the row from precision. `report.json` gains `scored_rater`.
- The coverage curve weights each labelled row by its stratum population ÷
  rows labelled in that stratum instead of the sample's stored `weight`, so
  a subset or a partly filled sheet no longer skews coverage precision
  toward the source sample's allocation (#111).
