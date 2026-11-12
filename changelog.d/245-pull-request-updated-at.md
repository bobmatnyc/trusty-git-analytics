Breaking
- The public `PullRequest` struct gains `updated_at: Option<DateTime<Utc>>`, and `GithubConfig` gains `review_fetch_max_prs: u32` (`GithubConfig` is `#[non_exhaustive]`, so only `PullRequest` struct literals outside the crate break). Set `updated_at: None` where the provider reports no update time ([#245](https://github.com/bobmatnyc/trusty-git-analytics/issues/245)).
