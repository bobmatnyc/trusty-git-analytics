Breaking
- tga 10.0.0: the public config, stats and summary types are now
  `#[non_exhaustive]` (#137), so a new field or variant is an additive change
  and no longer forces a major release. YAML config deserialization is
  unchanged. The affected groups:
  - Config: `Config` and every section — `RepositoryConfig`, `TeamConfig`,
    `TeamMember`, `OutputConfig`, `ClassificationConfig`, `ReachabilityConfig`,
    `LinearConfig`, `BitbucketConfig`, `PmConfig`, `AzureDevOpsConfig`,
    `DoraConfig`, `FailureSignal`, `AnalysisConfig`, `MlCategorizationConfig`,
    `CacheConfig`, `LlmConfig`, `AliasFile`, `DeveloperAliasEntry` — plus
    `ClassificationEngineConfig`, `WeightedSumConfig`, `SubcategoryDef`,
    `Rule`, `RuleSet`, `CategoryDef`, `SourceConfig` and each
    `*SourceConfig` / `*FieldMappings`, `DiffSamplerConfig` and
    `GithubIssueConfig`.
  - Stats and usage: `ClassificationStats`, `RepoCoverage`, `LlmUsageTotals`,
    `LlmUsage`, `LlmCall`, `CollectionStats`, `ReachabilityStats`,
    `EffortCounts`, `ReportStats`, `TicketedStats`, `TokenCostSummary`.
  - Report models: every struct in `report::models` (`ReportData`,
    `ReportSummary`, `WeeklyMetrics`, `DoraMetrics`, …), the drill-down and
    DD-manifest models, `AuthorPeriodSummary` and `MonthlyActivity`.
  - Eval records and summaries: `SampleRecord`, `StrataSummary`,
    `SampleSummary`, `SubsampleSummary`, `RepredictSummary`, `ScoreReport`,
    `Kappa` and their row types.
  - Enums: `LlmSource`, `LlmEffort`, `LlmFallbackScope`, `LlmOutcome`,
    `SourceConfig`, `TraceTier`, `Stratum`, and the error enums `ConfigError`,
    `ClassifyError`, `ReportError`, `EvalError`.
- Migration for code outside the crate: a struct literal of these types no
  longer compiles, even with `..Default::default()`. Start from
  `Default::default()` and assign the public fields, deserialize from YAML, or
  use a constructor — new ones are `RepositoryConfig::new(path)`,
  `TeamMember::new(name, email)`, `Rule::new(id, category)`,
  `CategoryDef::new(name)`, `DeveloperAliasEntry::new(name, email)`,
  `AzureDevOpsConfig::new(url, pat)`, `JiraSourceConfig::new(base_url)`,
  `GithubIssuesSourceConfig::new(repo)`, `ConfluenceSourceConfig::new(base_url)`,
  and `LlmCall::answered(verdict, usage)`, `LlmCall::abstained(usage)` and
  `LlmCall::out_of_set(usage)` beside the existing `LlmCall::failed(usage)`.
  `SubcategoryDef` and `GithubIssueConfig` have no `Default`; build them with
  the existing `SubcategoryDef::new(name, parent)` and
  `GithubIssueConfig::from_slug(slug)`. Types that had no `Default` gained one
  where a struct needed it. Reading fields is unchanged. A `match` on one of the enums needs a wildcard (`_ =>`) arm.
- `Default` is not always the YAML default. `GithubConfig::default()` gives
  `fetch_on_reference: false`, `fetch_pr_reviews: false` and
  `review_fetch_concurrency: 0`, where an omitted YAML key gives `true`, `true`
  and `1`. `JiraConfig::default()` and `LinearConfig::default()` give
  `fetch_on_reference: false`, where YAML gives `true`. Set those fields
  explicitly when starting from `default()`. `TeamMember::default()` has an
  empty `email`, and two such members collide on the unique canonical email;
  use `TeamMember::new`. `RuleSet::default()` is an empty set, not the
  built-in rules (`tga::classify::rules::default_rules()`).
