Fixed

- A relative `classification.rules_file` in a config is now resolved against
  the config file's directory, as `database:` already was, instead of the
  process working directory (#111). `tga --config /abs/path/config.yaml` run
  from another directory no longer fails with an I/O error. Relative
  repository paths, `output.directory`, `cache.directory` and
  `dora.datadog_dir` follow the same rule, matching the Python predecessor. A
  repository configured as `path: .` with no `name` keeps `.` as its stored
  name.
