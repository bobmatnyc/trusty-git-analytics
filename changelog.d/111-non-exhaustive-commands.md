Changed

- **Breaking:** the public subcommand enums in `tga::commands` are now
  `#[non_exhaustive]` (#111): `EvalSubcommand`, `RulesSubcommand`,
  `OverrideSubcommand`, `InspectSubcommand`, `BackfillSubcommand`,
  `AliasesSubcommand`, `DeploymentsSubcommand`, `IncidentsSubcommand`,
  `JiraSubcommand` and `LinearSubcommand`. A downstream `match` on one of
  them needs a wildcard (`_ =>`) arm. In return, a future subcommand is an
  additive change and no longer forces a major release. The CLI itself is
  unchanged.
