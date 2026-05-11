//! Rule file loader and built-in default ruleset.

use std::path::Path;

use crate::classify::errors::{ClassifyError, Result};
use crate::classify::rules::types::{Rule, RuleSet};

/// Load a [`RuleSet`] from a YAML or JSON file.
///
/// Format is detected by extension (`.json` → JSON, anything else → YAML).
///
/// # Errors
///
/// - [`ClassifyError::Io`] if the file cannot be read.
/// - [`ClassifyError::Yaml`] / [`ClassifyError::Json`] on parse failure.
pub fn load_rules(path: &Path) -> Result<RuleSet> {
    let text = std::fs::read_to_string(path)?;
    let is_json = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    let set: RuleSet = if is_json {
        serde_json::from_str(&text)?
    } else {
        serde_yaml::from_str(&text)?
    };

    if set.rules.is_empty() {
        return Err(ClassifyError::RuleLoad(format!(
            "rule file {} contained no rules",
            path.display()
        )));
    }
    Ok(set)
}

/// Return the built-in default ruleset.
///
/// Covers a broad swath of commit-message patterns to keep the
/// "uncategorized" rate low without an LLM:
///
/// - Conventional commit prefixes (`feat:`, `fix:`, `chore:`, `docs:`,
///   `refactor:`, `test:`, `ci:`, `perf:`, `style:`, `build:`, `revert:`)
///   matched both as exact substrings (Tier 1) and as anchored regex
///   patterns at the start of the message (Tier 2). The regex form also
///   accepts optional scopes (`feat(api):`) and breaking-change markers
///   (`feat!:`).
/// - GitHub `Merge pull request #N from …` and `Merge branch …` headers.
/// - "Revert "..."" style headers (in addition to `revert:` prefix).
/// - Initial-commit / WIP / version-bump conventions.
/// - Dependency, infrastructure (Docker / k8s / Terraform / GitHub
///   Actions), and code-review keywords.
/// - JIRA-style ticket patterns (`PROJ-123`) and GitHub issue refs
///   (`#123`).
/// - Lower-priority keyword fallbacks for `bug`, `security`,
///   `performance`, etc. so that prose commit messages still classify
///   without LLM help.
pub fn default_rules() -> RuleSet {
    let rules = vec![
        // ================================================================
        // Tier-2 (regex) rules: anchored, conventional-commit prefixes.
        //
        // These run after exact keyword matches but are intentionally high
        // priority so that a leading `feat(scope)!:` beats a stray "bug"
        // word later in the message.
        // ================================================================
        Rule {
            id: "cc-feat".into(),
            category: "feature".into(),
            subcategory: None,
            keywords: vec!["feat:".into(), "feature:".into()],
            patterns: vec![r"(?i)^\s*feat(\([^)]*\))?!?:".into()],
            priority: 100,
            confidence: 0.95,
        },
        Rule {
            id: "cc-fix".into(),
            category: "bugfix".into(),
            subcategory: None,
            keywords: vec!["fix:".into(), "bugfix:".into(), "hotfix".into()],
            patterns: vec![r"(?i)^\s*fix(\([^)]*\))?!?:".into()],
            priority: 100,
            confidence: 0.95,
        },
        Rule {
            id: "cc-chore".into(),
            category: "chore".into(),
            subcategory: None,
            keywords: vec!["chore:".into()],
            patterns: vec![r"(?i)^\s*chore(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-docs".into(),
            category: "documentation".into(),
            subcategory: None,
            keywords: vec!["docs:".into(), "doc:".into()],
            patterns: vec![r"(?i)^\s*docs?(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-refactor".into(),
            category: "refactor".into(),
            subcategory: None,
            keywords: vec!["refactor:".into(), "refactoring:".into()],
            patterns: vec![r"(?i)^\s*refactor(ing)?(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-test".into(),
            category: "test".into(),
            subcategory: None,
            keywords: vec!["test:".into(), "tests:".into()],
            patterns: vec![r"(?i)^\s*tests?(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-ci".into(),
            category: "ci".into(),
            subcategory: None,
            keywords: vec!["ci:".into()],
            patterns: vec![r"(?i)^\s*ci(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-perf".into(),
            category: "performance".into(),
            subcategory: None,
            keywords: vec!["perf:".into(), "performance:".into()],
            patterns: vec![r"(?i)^\s*perf(ormance)?(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-style".into(),
            category: "style".into(),
            subcategory: None,
            keywords: vec!["style:".into()],
            patterns: vec![r"(?i)^\s*style(\([^)]*\))?!?:".into()],
            priority: 80,
            confidence: 0.85,
        },
        Rule {
            id: "cc-build".into(),
            category: "build".into(),
            subcategory: None,
            keywords: vec!["build:".into()],
            patterns: vec![r"(?i)^\s*build(\([^)]*\))?!?:".into()],
            priority: 80,
            confidence: 0.85,
        },
        Rule {
            id: "cc-revert".into(),
            category: "revert".into(),
            subcategory: None,
            // Include the leading word with trailing space so that an
            // auto-generated `Revert "feat: ..."` message wins the Tier-1
            // race against the inner `feat:` keyword.
            keywords: vec!["revert:".into(), "revert \"".into()],
            patterns: vec![
                r"(?i)^\s*revert(\([^)]*\))?!?:".into(),
                r#"(?i)^\s*revert\s+""#.into(), // "Revert "feat: ..."" auto-generated
                r"(?i)^\s*this reverts commit".into(),
            ],
            // Above `cc-feat` (100) and `cc-fix` (100) so that
            // `Revert "feat: ..."` is classified as a revert, not a feature.
            priority: 115,
            confidence: 0.9,
        },
        // Additional conventional-style prefixes seen in the wild.
        Rule {
            id: "cc-security".into(),
            category: "security".into(),
            subcategory: None,
            keywords: vec!["security:".into(), "sec:".into()],
            patterns: vec![r"(?i)^\s*(security|sec)(\([^)]*\))?!?:".into()],
            priority: 95,
            confidence: 0.9,
        },
        Rule {
            id: "cc-deps".into(),
            category: "maintenance".into(),
            subcategory: Some("dependencies".into()),
            keywords: vec!["deps:".into(), "dep:".into(), "dependencies:".into()],
            patterns: vec![r"(?i)^\s*dep(s|endencies)?(\([^)]*\))?!?:".into()],
            priority: 85,
            confidence: 0.9,
        },
        Rule {
            id: "cc-i18n".into(),
            category: "localization".into(),
            subcategory: None,
            keywords: vec!["i18n:".into(), "l10n:".into()],
            patterns: vec![r"(?i)^\s*(i18n|l10n)(\([^)]*\))?!?:".into()],
            priority: 85,
            confidence: 0.9,
        },
        Rule {
            id: "cc-release".into(),
            category: "release".into(),
            subcategory: None,
            keywords: vec!["release:".into()],
            patterns: vec![r"(?i)^\s*release(\([^)]*\))?!?:".into()],
            priority: 85,
            confidence: 0.9,
        },
        Rule {
            id: "cc-wip".into(),
            category: "wip".into(),
            subcategory: None,
            keywords: vec!["wip:".into()],
            patterns: vec![
                r"(?i)^\s*wip(\([^)]*\))?!?:".into(),
                r"(?i)^\s*\[wip\]".into(),
            ],
            priority: 85,
            confidence: 0.85,
        },
        // ================================================================
        // Breaking-change marker (highest priority — overrides cc-feat etc.)
        // ================================================================
        Rule {
            id: "breaking-change".into(),
            category: "breaking".into(),
            subcategory: Some("api".into()),
            keywords: vec!["breaking change".into(), "breaking-change".into()],
            patterns: vec![
                r"(?i)breaking[\s-]change".into(),
                // Conventional-commit `!:` breaking marker
                // e.g. `feat(api)!: drop v1`, `refactor!: rename module`.
                r"(?i)^\s*(feat|fix|chore|refactor|perf|build|docs|ci|style|test)(\([^)]*\))?!:"
                    .into(),
            ],
            priority: 110,
            confidence: 0.9,
        },
        // ================================================================
        // Merge / git-plumbing patterns (the fuzzy tier also handles these,
        // but having rules catches them earlier with higher confidence).
        // ================================================================
        Rule {
            id: "merge-pr".into(),
            category: "merge".into(),
            subcategory: Some("pull-request".into()),
            keywords: vec!["merge pull request".into(), "merge remote-tracking".into()],
            patterns: vec![r"(?i)^\s*merge pull request #\d+".into()],
            priority: 105,
            confidence: 0.95,
        },
        Rule {
            id: "merge-branch".into(),
            category: "merge".into(),
            subcategory: Some("branch".into()),
            keywords: vec!["merge branch".into()],
            patterns: vec![
                r"(?i)^\s*merge branch ".into(),
                r"(?i)^\s*merge tag ".into(),
            ],
            priority: 105,
            confidence: 0.95,
        },
        // ================================================================
        // Initial / bootstrap / repo-setup commits.
        // ================================================================
        Rule {
            id: "initial-commit".into(),
            category: "chore".into(),
            subcategory: Some("initial".into()),
            keywords: vec!["initial commit".into(), "first commit".into()],
            patterns: vec![
                r"(?i)^\s*initial\s+commit\b".into(),
                r"(?i)^\s*first\s+commit\b".into(),
                r"(?i)^\s*initial\s+import\b".into(),
                r"(?i)^\s*bootstrap\s+repo".into(),
            ],
            priority: 95,
            confidence: 0.9,
        },
        // ================================================================
        // Version-bump / release tagging.
        // ================================================================
        Rule {
            id: "version-bump".into(),
            category: "release".into(),
            subcategory: Some("version-bump".into()),
            keywords: vec![
                "bump version".into(),
                "version bump".into(),
                "release version".into(),
                "prepare release".into(),
                "cut release".into(),
            ],
            patterns: vec![
                r"(?i)^\s*bump\s+(version|to\s+v?\d)".into(),
                r"(?i)^\s*release\s+v?\d+\.\d+".into(),
                r"(?i)^\s*v?\d+\.\d+\.\d+(\s*$|\s+release)".into(),
            ],
            priority: 90,
            confidence: 0.9,
        },
        // ================================================================
        // Dependency updates — very common "uncategorized" source.
        // ================================================================
        Rule {
            id: "kw-deps-update".into(),
            category: "maintenance".into(),
            subcategory: Some("dependencies".into()),
            keywords: vec![
                "update deps".into(),
                "update dependencies".into(),
                "upgrade deps".into(),
                "upgrade dependencies".into(),
                "bump deps".into(),
                "bump dependencies".into(),
                "pin dependencies".into(),
                "lockfile".into(),
                "package-lock".into(),
                "yarn.lock".into(),
                "cargo.lock".into(),
                "poetry.lock".into(),
            ],
            patterns: vec![
                r"(?i)\bbump\s+\S+\s+from\s+\S+\s+to\s+\S+".into(), // Dependabot
                r"(?i)\bupdate\s+\S+\s+to\s+v?\d+\.\d+".into(),
            ],
            priority: 75,
            confidence: 0.9,
        },
        Rule {
            id: "kw-dependabot".into(),
            category: "maintenance".into(),
            subcategory: Some("dependencies".into()),
            keywords: vec!["dependabot".into(), "renovate".into(), "snyk".into()],
            patterns: vec![],
            priority: 75,
            confidence: 0.9,
        },
        // ================================================================
        // Lint / formatting / tooling cleanup.
        // ================================================================
        Rule {
            id: "kw-lint".into(),
            category: "style".into(),
            subcategory: Some("lint".into()),
            keywords: vec![
                "fix lint".into(),
                "lint fix".into(),
                "fix linting".into(),
                "fix linter".into(),
                "satisfy lint".into(),
                "clippy fix".into(),
                "fix clippy".into(),
                "eslint fix".into(),
                "rubocop".into(),
                "prettier".into(),
                "gofmt".into(),
                "rustfmt".into(),
                "black format".into(),
            ],
            patterns: vec![r"(?i)\bfix(es|ed|ing)?\s+lint(ing|er)?\b".into()],
            priority: 75,
            confidence: 0.85,
        },
        Rule {
            id: "kw-format".into(),
            category: "style".into(),
            subcategory: Some("format".into()),
            keywords: vec![
                "reformat".into(),
                "code formatting".into(),
                "fix formatting".into(),
                "fix whitespace".into(),
                "trailing whitespace".into(),
                "fix indentation".into(),
            ],
            patterns: vec![],
            priority: 65,
            confidence: 0.8,
        },
        // ================================================================
        // Code review / PR feedback follow-ups.
        // ================================================================
        Rule {
            id: "kw-review".into(),
            category: "refactor".into(),
            subcategory: Some("review".into()),
            keywords: vec![
                "address review".into(),
                "address feedback".into(),
                "address comments".into(),
                "review feedback".into(),
                "review comments".into(),
                "pr feedback".into(),
                "code review".into(),
                "apply suggestions".into(),
                "incorporate review".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.8,
        },
        // ================================================================
        // Cleanup / housekeeping prose.
        // ================================================================
        Rule {
            id: "kw-cleanup".into(),
            category: "refactor".into(),
            subcategory: Some("cleanup".into()),
            keywords: vec![
                "clean up".into(),
                "cleanup".into(),
                "dead code".into(),
                "remove unused".into(),
                "delete unused".into(),
                "tidy up".into(),
                "housekeeping".into(),
            ],
            patterns: vec![r"(?i)\bremove\s+(unused|dead|stale|obsolete)\b".into()],
            priority: 60,
            confidence: 0.8,
        },
        // ================================================================
        // Infrastructure / DevOps / CI keywords.
        // ================================================================
        Rule {
            id: "kw-docker".into(),
            category: "build".into(),
            subcategory: Some("docker".into()),
            keywords: vec![
                "dockerfile".into(),
                "docker-compose".into(),
                "docker compose".into(),
                "docker image".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        Rule {
            id: "kw-k8s".into(),
            category: "build".into(),
            subcategory: Some("kubernetes".into()),
            keywords: vec![
                "kubernetes".into(),
                "k8s".into(),
                "helm chart".into(),
                "kustomize".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        Rule {
            id: "kw-terraform".into(),
            category: "build".into(),
            subcategory: Some("terraform".into()),
            keywords: vec![
                "terraform".into(),
                "tf module".into(),
                "tflint".into(),
                "ansible playbook".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        Rule {
            id: "kw-github-actions".into(),
            category: "ci".into(),
            subcategory: Some("github-actions".into()),
            keywords: vec![
                "github action".into(),
                "github actions".into(),
                "github workflow".into(),
                "gh action".into(),
                ".github/workflows".into(),
                "circleci".into(),
                "gitlab ci".into(),
                "jenkinsfile".into(),
                "azure pipeline".into(),
                "azure pipelines".into(),
                "travis".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        // ================================================================
        // Generic verbs — lowest priority so prefix rules win first.
        // These convert prose commits into reasonable categories.
        // ================================================================
        Rule {
            id: "kw-add-implement".into(),
            category: "feature".into(),
            subcategory: None,
            keywords: vec![
                "implement".into(),
                "introduce".into(),
                "add support".into(),
                "add feature".into(),
                "new feature".into(),
                "initial implementation".into(),
            ],
            patterns: vec![r"(?i)^\s*add\s+(new\s+)?(support\s+for|feature|the)\b".into()],
            priority: 45,
            confidence: 0.7,
        },
        Rule {
            id: "kw-fix-resolve".into(),
            category: "bugfix".into(),
            subcategory: None,
            keywords: vec![
                "fix bug".into(),
                "fix issue".into(),
                "fix crash".into(),
                "fix regression".into(),
                "fix race".into(),
                "fix deadlock".into(),
                "fix leak".into(),
                "fix segfault".into(),
                "fix panic".into(),
                "fix error".into(),
                "resolve issue".into(),
                "resolve bug".into(),
                "resolve race".into(),
                "resolve deadlock".into(),
                "fixes #".into(),
                "fixes:".into(),
                "closes #".into(),
                "patch bug".into(),
                "correct behavior".into(),
                "correct handling".into(),
            ],
            patterns: vec![r"(?i)\b(fix(es|ed)?|resolves?|closes?)\s+#\d+".into()],
            priority: 60,
            confidence: 0.85,
        },
        Rule {
            id: "kw-bug".into(),
            category: "bugfix".into(),
            subcategory: None,
            keywords: vec!["defect".into(), "regression".into()],
            patterns: vec![r"(?i)\b(bug|bugs)\b".into()],
            priority: 40,
            confidence: 0.7,
        },
        Rule {
            id: "kw-security".into(),
            category: "security".into(),
            subcategory: None,
            keywords: vec![
                "security patch".into(),
                "security fix".into(),
                "vulnerability".into(),
                "cve-".into(),
                "xss".into(),
                "csrf".into(),
                "sql injection".into(),
                "rce".into(),
                "ssrf".into(),
            ],
            patterns: vec![r"(?i)\bCVE-\d{4}-\d+".into()],
            priority: 80,
            confidence: 0.9,
        },
        Rule {
            id: "kw-performance".into(),
            category: "performance".into(),
            subcategory: None,
            keywords: vec![
                "speed up".into(),
                "speedup".into(),
                "optimize".into(),
                "optimization".into(),
                "improve performance".into(),
                "reduce latency".into(),
                "reduce memory".into(),
                "memory leak".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
        Rule {
            id: "kw-docs".into(),
            category: "documentation".into(),
            subcategory: None,
            keywords: vec![
                "readme".into(),
                "changelog".into(),
                "update docs".into(),
                "documentation".into(),
                "javadoc".into(),
                "rustdoc".into(),
                "docstring".into(),
                "doc comment".into(),
            ],
            patterns: vec![r"(?i)\bupdate\s+(the\s+)?(readme|changelog|docs)\b".into()],
            priority: 50,
            confidence: 0.8,
        },
        Rule {
            id: "kw-test-add".into(),
            category: "test".into(),
            subcategory: None,
            keywords: vec![
                "add test".into(),
                "add tests".into(),
                "unit test".into(),
                "unit tests".into(),
                "integration test".into(),
                "e2e test".into(),
                "snapshot test".into(),
                "test coverage".into(),
                "test suite".into(),
                "fix test".into(),
                "fix tests".into(),
                "fix flaky".into(),
                "flaky test".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.85,
        },
        Rule {
            id: "kw-config".into(),
            category: "chore".into(),
            subcategory: Some("config".into()),
            keywords: vec![
                "update config".into(),
                "config change".into(),
                "configuration".into(),
                ".gitignore".into(),
                ".editorconfig".into(),
                "tsconfig".into(),
                "pyproject".into(),
                "package.json".into(),
                "cargo.toml".into(),
            ],
            patterns: vec![],
            priority: 50,
            confidence: 0.75,
        },
        Rule {
            id: "kw-database".into(),
            category: "feature".into(),
            subcategory: Some("database".into()),
            keywords: vec![
                "db migration".into(),
                "database migration".into(),
                "schema migration".into(),
                "add migration".into(),
                "new migration".into(),
                "alter table".into(),
            ],
            patterns: vec![],
            priority: 60,
            confidence: 0.8,
        },
        Rule {
            id: "kw-wip".into(),
            category: "wip".into(),
            subcategory: None,
            keywords: vec![
                "work in progress".into(),
                "todo:".into(),
                "fixme:".into(),
                "checkpoint".into(),
            ],
            patterns: vec![r"(?i)^\s*wip\b".into(), r"(?i)^\s*\[wip\]".into()],
            priority: 40,
            confidence: 0.65,
        },
        // ================================================================
        // Cloud platforms (AWS / GCP / Azure).
        // ================================================================
        Rule {
            id: "kw-cloud-aws".into(),
            category: "cloud".into(),
            subcategory: Some("aws".into()),
            keywords: vec![
                "aws ".into(),
                " aws".into(),
                "amazon web services".into(),
                "cloudformation".into(),
                "cloudfront".into(),
                "cloudwatch".into(),
                " s3 ".into(),
                "s3 bucket".into(),
                " ec2".into(),
                " lambda".into(),
                "aws lambda".into(),
                " eks".into(),
                " ecs".into(),
                " rds".into(),
                " sqs".into(),
                " sns".into(),
                "iam role".into(),
                "iam policy".into(),
                "route53".into(),
                "route 53".into(),
                "dynamodb".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        Rule {
            id: "kw-cloud-gcp".into(),
            category: "cloud".into(),
            subcategory: Some("gcp".into()),
            keywords: vec![
                " gcp".into(),
                "google cloud".into(),
                "bigquery".into(),
                "cloud run".into(),
                "cloud functions".into(),
                " gke".into(),
                "pub/sub".into(),
                "pubsub".into(),
                "gcs bucket".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        Rule {
            id: "kw-cloud-azure".into(),
            category: "cloud".into(),
            subcategory: Some("azure".into()),
            keywords: vec![
                " azure".into(),
                "azure functions".into(),
                "azure devops".into(),
                " aks".into(),
                "app service".into(),
                "blob storage".into(),
                "cosmos db".into(),
                "cosmosdb".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        // ================================================================
        // Monitoring / observability.
        // ================================================================
        Rule {
            id: "kw-monitoring".into(),
            category: "monitoring".into(),
            subcategory: None,
            keywords: vec![
                "datadog".into(),
                "prometheus".into(),
                "grafana".into(),
                "sentry".into(),
                "newrelic".into(),
                "new relic".into(),
                "pagerduty".into(),
                "splunk".into(),
                "opentelemetry".into(),
                "otel".into(),
                "tracing".into(),
                "metrics".into(),
                "alerting".into(),
                "alert rule".into(),
                "dashboard".into(),
                "log aggregation".into(),
                "elk stack".into(),
                "kibana".into(),
                "logstash".into(),
            ],
            patterns: vec![],
            priority: 65,
            confidence: 0.8,
        },
        // ================================================================
        // Databases.
        // ================================================================
        Rule {
            id: "kw-database".into(),
            category: "database".into(),
            subcategory: None,
            keywords: vec![
                "postgresql".into(),
                "postgres".into(),
                " mysql".into(),
                "mariadb".into(),
                "sqlite".into(),
                " redis".into(),
                "mongodb".into(),
                "mongo db".into(),
                "elasticsearch".into(),
                "cassandra".into(),
                "dynamodb".into(),
                "schema change".into(),
                "schema migration".into(),
                "db schema".into(),
                "database schema".into(),
                "index migration".into(),
            ],
            patterns: vec![],
            priority: 65,
            confidence: 0.8,
        },
        // ================================================================
        // Messaging / queues.
        // ================================================================
        Rule {
            id: "kw-messaging".into(),
            category: "messaging".into(),
            subcategory: None,
            keywords: vec![
                " kafka".into(),
                "rabbitmq".into(),
                "rabbit mq".into(),
                " sqs".into(),
                " sns".into(),
                "pub/sub".into(),
                "pubsub".into(),
                "nats".into(),
                "amqp".into(),
                "message queue".into(),
                "event bus".into(),
                "event stream".into(),
            ],
            patterns: vec![],
            priority: 65,
            confidence: 0.8,
        },
        // ================================================================
        // Networking / web infra.
        // ================================================================
        Rule {
            id: "kw-networking".into(),
            category: "networking".into(),
            subcategory: None,
            keywords: vec![
                " nginx".into(),
                "traefik".into(),
                "load balancer".into(),
                " cdn".into(),
                " ssl".into(),
                " tls".into(),
                "certificate".into(),
                "cert manager".into(),
                "letsencrypt".into(),
                "let's encrypt".into(),
                "reverse proxy".into(),
                "ingress controller".into(),
                "service mesh".into(),
                " istio".into(),
                "linkerd".into(),
                "envoy proxy".into(),
            ],
            patterns: vec![],
            priority: 65,
            confidence: 0.8,
        },
        // ================================================================
        // Language / ecosystem tooling.
        // ================================================================
        Rule {
            id: "kw-rust-tooling".into(),
            category: "tooling".into(),
            subcategory: Some("rust".into()),
            keywords: vec![
                " cargo ".into(),
                "cargo run".into(),
                "cargo test".into(),
                "cargo build".into(),
                "cargo clippy".into(),
                " clippy".into(),
                "rustfmt".into(),
                "cargo.toml".into(),
                "rust crate".into(),
                "rust workspace".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
        Rule {
            id: "kw-js-tooling".into(),
            category: "tooling".into(),
            subcategory: Some("javascript".into()),
            keywords: vec![
                " npm ".into(),
                "npm install".into(),
                "npm run".into(),
                " yarn ".into(),
                " pnpm".into(),
                "package.json".into(),
                "node_modules".into(),
                "webpack".into(),
                " vite ".into(),
                " vitest".into(),
                "eslint".into(),
                "prettier".into(),
                "tsconfig".into(),
                "tsc build".into(),
                "babel".into(),
                "rollup".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
        Rule {
            id: "kw-python-tooling".into(),
            category: "tooling".into(),
            subcategory: Some("python".into()),
            keywords: vec![
                " poetry ".into(),
                " pip ".into(),
                "pip install".into(),
                "pyproject".into(),
                "virtualenv".into(),
                " venv".into(),
                " conda".into(),
                "requirements.txt".into(),
                "setup.py".into(),
                " ruff".into(),
                " mypy".into(),
                " pytest".into(),
                " tox".into(),
                " uv ".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
        Rule {
            id: "kw-java-tooling".into(),
            category: "tooling".into(),
            subcategory: Some("java".into()),
            keywords: vec![
                " maven".into(),
                " gradle".into(),
                "pom.xml".into(),
                "build.gradle".into(),
                "spring boot".into(),
                "springboot".into(),
                " jvm".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
        Rule {
            id: "kw-go-tooling".into(),
            category: "tooling".into(),
            subcategory: Some("go".into()),
            keywords: vec![
                "go.mod".into(),
                "go.sum".into(),
                "goroutine".into(),
                "gofmt".into(),
                "go modules".into(),
                "go vet".into(),
                "golangci".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
        // ================================================================
        // PR hygiene / cleanup of debug artifacts.
        // ================================================================
        Rule {
            id: "kw-pr-hygiene".into(),
            category: "refactor".into(),
            subcategory: Some("cleanup".into()),
            keywords: vec![
                "remove debug".into(),
                "remove console.log".into(),
                "remove console log".into(),
                "remove print".into(),
                "remove todo".into(),
                "remove fixme".into(),
                "remove commented".into(),
                "remove logging".into(),
                "drop debug".into(),
                "strip debug".into(),
                "nit:".into(),
                " nits".into(),
                "per review".into(),
                "per cr".into(),
                "reviewer feedback".into(),
                "suggested changes".into(),
            ],
            patterns: vec![],
            priority: 60,
            confidence: 0.8,
        },
        // ================================================================
        // Experiments / spikes / prototypes.
        // ================================================================
        Rule {
            id: "kw-experiment".into(),
            category: "experiment".into(),
            subcategory: None,
            keywords: vec![
                "experiment".into(),
                "experimental".into(),
                " spike ".into(),
                " spike:".into(),
                "proof of concept".into(),
                " poc ".into(),
                " poc:".into(),
                "prototype".into(),
                "prototyping".into(),
                "try out".into(),
                "trying out".into(),
            ],
            patterns: vec![],
            priority: 50,
            confidence: 0.75,
        },
        // ================================================================
        // Rollback (not the `revert:` prefix — prose form).
        // ================================================================
        Rule {
            id: "kw-rollback".into(),
            category: "rollback".into(),
            subcategory: None,
            keywords: vec![
                "rollback".into(),
                "roll back".into(),
                " undo ".into(),
                "revert to".into(),
                "back out".into(),
                "backed out".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        // ================================================================
        // Auto-generated git plumbing.
        // ================================================================
        Rule {
            id: "kw-auto-generated".into(),
            category: "maintenance".into(),
            subcategory: Some("auto-generated".into()),
            keywords: vec![
                "squashed commit".into(),
                "cherry pick".into(),
                "cherry-pick".into(),
                "cherry-picked".into(),
                "auto-merge".into(),
                "automerge".into(),
                "auto generated".into(),
                "auto-generated".into(),
            ],
            patterns: vec![],
            priority: 80,
            confidence: 0.9,
        },
        // ================================================================
        // Translations / i18n / l10n prose.
        // ================================================================
        Rule {
            id: "kw-translation".into(),
            category: "translation".into(),
            subcategory: None,
            keywords: vec![
                "translation".into(),
                "translations".into(),
                "translate".into(),
                "translated".into(),
                "localize".into(),
                "localization".into(),
                "localisation".into(),
                " locale".into(),
                "locale file".into(),
                " i18n".into(),
                " l10n".into(),
                "language file".into(),
            ],
            patterns: vec![],
            priority: 60,
            confidence: 0.85,
        },
        // ================================================================
        // Repo-meta documentation files.
        // ================================================================
        Rule {
            id: "kw-repo-meta".into(),
            category: "documentation".into(),
            subcategory: Some("repo-meta".into()),
            keywords: vec![
                "contributing".into(),
                "code_of_conduct".into(),
                "code of conduct".into(),
                "license file".into(),
                "license.md".into(),
                "license.txt".into(),
                "security.md".into(),
                "support.md".into(),
                "authors.md".into(),
                "maintainers.md".into(),
                "history.md".into(),
                "news.md".into(),
                "releases.md".into(),
            ],
            patterns: vec![],
            priority: 60,
            confidence: 0.85,
        },
        // ================================================================
        // API docs / specs.
        // ================================================================
        Rule {
            id: "kw-api-docs".into(),
            category: "documentation".into(),
            subcategory: Some("api".into()),
            keywords: vec![
                "swagger".into(),
                "openapi".into(),
                "open api".into(),
                "postman".into(),
                "api docs".into(),
                "api documentation".into(),
                "jsdoc".into(),
                "tsdoc".into(),
                "rustdoc".into(),
                "javadoc".into(),
                "docstring".into(),
                "doc comment".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.85,
        },
        // ================================================================
        // Website / marketing / landing content.
        // ================================================================
        Rule {
            id: "kw-content".into(),
            category: "content-docs".into(),
            subcategory: None,
            keywords: vec![
                "landing page".into(),
                "blog post".into(),
                "blogpost".into(),
                "announcement".into(),
                "marketing copy".into(),
                "copy update".into(),
                "ui text".into(),
                "ui copy".into(),
                "microcopy".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
        // ================================================================
        // Assets (images, icons, fonts, svg).
        // ================================================================
        Rule {
            id: "kw-assets".into(),
            category: "assets".into(),
            subcategory: None,
            keywords: vec![
                " svg".into(),
                " png".into(),
                " jpg".into(),
                " jpeg".into(),
                " gif".into(),
                " webp".into(),
                "favicon".into(),
                " icons".into(),
                "icon set".into(),
                " font ".into(),
                "fonts/".into(),
                "logo".into(),
            ],
            patterns: vec![],
            priority: 40,
            confidence: 0.7,
        },
        // ================================================================
        // Generic single-word / very-short prose verbs (low priority, low conf).
        // These are the second-to-last safety net before the catch-all.
        // ================================================================
        Rule {
            id: "kw-generic-add".into(),
            category: "feature".into(),
            subcategory: None,
            keywords: vec![],
            patterns: vec![
                r"(?i)^\s*add(s|ed|ing)?\b".into(),
                r"(?i)^\s*create(s|d|ing)?\b".into(),
                r"(?i)^\s*introduce(s|d|ing)?\b".into(),
                r"(?i)^\s*support(s|ed|ing)?\b".into(),
                r"(?i)^\s*enable(s|d|ing)?\b".into(),
                r"(?i)^\s*allow(s|ed|ing)?\b".into(),
            ],
            priority: 20,
            confidence: 0.55,
        },
        Rule {
            id: "kw-generic-update".into(),
            category: "maintenance".into(),
            subcategory: None,
            keywords: vec![],
            patterns: vec![
                r"(?i)^\s*update(s|d|ing)?\b".into(),
                r"(?i)^\s*modif(y|ies|ied|ying)\b".into(),
                r"(?i)^\s*change(s|d)?\b".into(),
                r"(?i)^\s*adjust(s|ed|ing)?\b".into(),
                r"(?i)^\s*tweak(s|ed|ing)?\b".into(),
                r"(?i)^\s*tune(s|d|ing)?\b".into(),
                r"(?i)^\s*edit(s|ed|ing)?\b".into(),
                r"(?i)^\s*rename(s|d|ing)?\b".into(),
                r"(?i)^\s*move(s|d|ing)?\b".into(),
                r"(?i)^\s*replace(s|d|ing)?\b".into(),
                r"(?i)^\s*switch(es|ed|ing)?\b".into(),
                r"(?i)^\s*upgrade(s|d|ing)?\b".into(),
                r"(?i)^\s*bump(s|ed|ing)?\b".into(),
                r"(?i)^\s*improve(s|d|ing)?\b".into(),
                r"(?i)^\s*enhance(s|d|ing)?\b".into(),
                r"(?i)^\s*polish(es|ed|ing)?\b".into(),
            ],
            priority: 18,
            confidence: 0.55,
        },
        Rule {
            id: "kw-generic-remove".into(),
            category: "refactor".into(),
            subcategory: Some("cleanup".into()),
            keywords: vec![],
            patterns: vec![
                r"(?i)^\s*remove(s|d|ing)?\b".into(),
                r"(?i)^\s*delete(s|d|ing)?\b".into(),
                r"(?i)^\s*drop(s|ped|ping)?\b".into(),
                r"(?i)^\s*strip(s|ped|ping)?\b".into(),
                r"(?i)^\s*purge(s|d|ing)?\b".into(),
                r"(?i)^\s*deprecate(s|d|ing)?\b".into(),
            ],
            priority: 18,
            confidence: 0.6,
        },
        Rule {
            id: "kw-generic-fix".into(),
            category: "bugfix".into(),
            subcategory: None,
            keywords: vec![],
            patterns: vec![
                r"(?i)^\s*fix(es|ed|ing)?\b".into(),
                r"(?i)^\s*correct(s|ed|ing)?\b".into(),
                r"(?i)^\s*repair(s|ed|ing)?\b".into(),
                r"(?i)^\s*patch(es|ed|ing)?\b".into(),
                r"(?i)^\s*handle(s|d|ing)?\b".into(),
                r"(?i)^\s*prevent(s|ed|ing)?\b".into(),
                r"(?i)^\s*avoid(s|ed|ing)?\b".into(),
            ],
            priority: 22,
            confidence: 0.6,
        },
        Rule {
            id: "kw-single-word".into(),
            category: "maintenance".into(),
            subcategory: None,
            keywords: vec![],
            patterns: vec![
                r"(?i)^\s*wip\s*\.?\s*$".into(),
                r"(?i)^\s*fix\s*\.?\s*$".into(),
                r"(?i)^\s*update\s*\.?\s*$".into(),
                r"(?i)^\s*updates\s*\.?\s*$".into(),
                r"(?i)^\s*changes?\s*\.?\s*$".into(),
                r"(?i)^\s*cleanup\s*\.?\s*$".into(),
                r"(?i)^\s*tweak\s*\.?\s*$".into(),
                r"(?i)^\s*edit\s*\.?\s*$".into(),
                r"(?i)^\s*minor\s*\.?\s*$".into(),
                r"(?i)^\s*misc\s*\.?\s*$".into(),
                r"(?i)^\s*temp\s*\.?\s*$".into(),
                r"(?i)^\s*testing\s*\.?\s*$".into(),
            ],
            priority: 25,
            confidence: 0.5,
        },
        // ================================================================
        // Bare ticket-only message (e.g. "PROJ-123" or "PROJ-456 some work").
        // The standalone "jira-ticket" rule below also matches, but this one
        // has explicit subcategory routing through "maintenance".
        // ================================================================
        Rule {
            id: "bare-ticket-prefix".into(),
            category: "maintenance".into(),
            subcategory: Some("ticketed".into()),
            keywords: vec![],
            patterns: vec![r"(?i)^\s*[A-Z][A-Z0-9]+-\d+([:\s].*)?$".into()],
            priority: 15,
            confidence: 0.5,
        },
        // ================================================================
        // Ticket / issue identifiers (regex tier).
        // ================================================================
        Rule {
            id: "jira-ticket".into(),
            category: "feature".into(),
            subcategory: Some("ticketed".into()),
            keywords: vec![],
            patterns: vec![r"\b[A-Z][A-Z0-9]+-\d+\b".into()],
            priority: 30,
            confidence: 0.7,
        },
        Rule {
            id: "github-issue-ref".into(),
            category: "feature".into(),
            subcategory: Some("issue".into()),
            keywords: vec![],
            patterns: vec![r"(?i)(^|\s)(refs?|references|see|for)\s+#\d+\b".into()],
            priority: 25,
            confidence: 0.6,
        },
        // ================================================================
        // Final catch-all safety net.
        //
        // Lowest possible priority — matches any non-empty message after
        // every other rule has had its chance. Confidence is intentionally
        // low (0.3) so downstream reports can flag these for LLM review.
        //
        // Routes to subcategory "uncategorized" (Unknown top-level) so the
        // result is still semantically marked as unclassified, but the
        // commit produces a deterministic verdict instead of falling
        // through to the fuzzy/LLM tiers (which can be slow or unavailable).
        // ================================================================
        Rule {
            id: "catch-all".into(),
            category: "maintenance".into(),
            subcategory: Some("uncategorized".into()),
            keywords: vec![],
            patterns: vec![r"(?s).+".into()],
            priority: 1,
            confidence: 0.3,
        },
    ];

    RuleSet {
        version: Some("1.0".into()),
        extend_defaults: true,
        rules,
    }
}
