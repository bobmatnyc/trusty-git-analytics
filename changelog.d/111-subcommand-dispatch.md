Added

- `DeploymentsSubcommandArgs::run`, `IncidentsSubcommandArgs::run`,
  `JiraSubcommandArgs::run` and `LinearSubcommandArgs::run` in
  `tga::commands::args` dispatch the selected operation (#111). Library
  callers use them instead of matching the now `#[non_exhaustive]`
  subcommand enums, so a new operation reaches them without a code change.
