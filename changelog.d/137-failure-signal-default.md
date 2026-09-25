Fixed
- `FailureSignal::default()` now gives `within_hours: 48`, the value an
  omitted YAML key gives (#137). The derived `Default` gave 0, a window that
  matches no commit, so a signal built in code read a 0% change-failure rate
  with no error.
