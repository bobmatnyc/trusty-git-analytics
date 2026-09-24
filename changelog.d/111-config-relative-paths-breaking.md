Changed

- **Breaking:** relative paths in a config file now resolve from the config
  file's directory, as the Python tool does, not from the working directory
  (#111). This covers `repositories[].path`, `output.directory`,
  `cache.directory`, `dora.datadog_dir` and `classification.rules_file`. A
  shared `~/cfg/tga.yaml` with `path: .`, run from inside each repository,
  now collects `~/cfg`. Use absolute paths to keep the old behaviour.
  Repository names derived from a path, and the GitHub slug lookup for a
  repository with no `name`, still use the path as written, so `path: .`
  keeps its stored name `.` and still finds its slug from the `origin`
  remote.
