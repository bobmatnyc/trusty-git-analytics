Changed

- **Breaking:** relative `output.directory`, `cache.directory` and
  `dora.datadog_dir` values in a config file now resolve from the config
  file's directory, as the Python tool does, not from the working directory
  (#111). A shared `~/cfg/tga.yaml` with `output: {directory: reports}`, run
  from inside each repository, now writes to `~/cfg/reports`. Use absolute
  paths to keep the old behaviour. `repositories[].path` is unchanged: a
  relative repository path still resolves from the working directory.
