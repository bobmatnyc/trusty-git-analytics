Breaking
- `ClassificationMethod` gains the variant `RepoMap` (stored as `repo_map`). The enum is not `#[non_exhaustive]`, so an exhaustive `match` on it outside the crate no longer compiles. `TraceTier::RepoMap` is added too; that enum is already `#[non_exhaustive]` (#158).
