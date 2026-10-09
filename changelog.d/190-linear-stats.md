Breaking
- `tga::core::config::LinearConfig` has a new public field, `stats` (`LinearStatsConfig`, the `linear.stats` YAML block). `LinearConfig` is `#[non_exhaustive]`; build it with `LinearConfig::default()` and set fields. `tga::commands::args::LinearSubcommand` gains a `Stats` variant; that enum is `#[non_exhaustive]`, so a `match` on it already needs a wildcard arm (#190).
