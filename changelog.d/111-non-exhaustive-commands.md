Changed

- **Breaking:** the public subcommand enums in `tga::commands` are now
  `#[non_exhaustive]` (#111): `EvalSubcommand`, `RulesSubcommand`,
  `OverrideSubcommand`, `InspectSubcommand`, `BackfillSubcommand`,
  `AliasesSubcommand`, `DeploymentsSubcommand`, `IncidentsSubcommand`,
  `JiraSubcommand` and `LinearSubcommand`. The same applies to the public
  clap value enums `ListFormat`, `AuthorFormat`, `InstallHost` and
  `InstallPm`. A downstream `match` on any of them needs a wildcard (`_ =>`)
  arm; constructing a variant still works. In return, a future subcommand or
  flag value is an additive change and no longer forces a major release. The
  CLI itself is unchanged.
