//! Azure DevOps integration configuration (Phase 1).
//!
//! Phase 1 establishes the configuration schema, validation, and the
//! `AB#(\d+)` ticket-reference regex. No HTTP calls are made — see
//! [`crate::collect::azdo`] for the stub client. Phase 2 will add the
//! HTTP session, an auth probe against `GET _apis/connectionData`, and
//! work-item fetching.
//!
//! # Design decisions (Phase 1)
//!
//! - **PAT-only authentication.** OAuth / Azure AD is deferred to Phase 2.
//! - **Cloud-only.** On-premises ADO Server (TFS) URLs are rejected at
//!   config load time with an explicit error. Only `dev.azure.com` and
//!   `*.visualstudio.com` are accepted.
//! - **Configurable work-item reference regex.** The default `AB#(\d+)`
//!   matches Microsoft's canonical convention, but real ADO orgs use a
//!   variety of conventions (`#NNNNNN`, `BUG #N`, …). Override via
//!   `pm.azure_devops.ticket_regex`. The regex must compile and must
//!   expose capture group 1, which is parsed as the numeric work-item
//!   ID for batch fetches. Validation happens at config-load time.
//!
//! Lives under `pm.azure_devops` in YAML (clean namespace; avoids the
//! `jira` / `jira_integration` dual-stack of the Python predecessor).

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::core::errors::TgaError;

/// Configuration for Microsoft Azure DevOps integration (Phase 1 — PAT auth, cloud only).
///
/// On-premises ADO Server (TFS) is not supported in Phase 1. Config validation
/// rejects non-cloud URLs at load time. Phase 2 will add OAuth and work-item fetching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzureDevOpsConfig {
    /// Azure DevOps organisation URL. Must be `https://dev.azure.com/{org}` or
    /// `https://{org}.visualstudio.com`. On-prem TFS/ADO Server URLs are rejected.
    pub organization_url: String,

    /// Personal Access Token. Supports `${AZURE_DEVOPS_PAT}` placeholder notation
    /// (note: tga does not interpolate env-vars — substitute before writing the config).
    /// Empty or whitespace-only values are rejected at validation time.
    pub pat: String,

    /// Azure DevOps project name (e.g. `"MyProject"`).
    pub project: String,

    /// Regex pattern used to detect ADO work-item references in commit messages.
    ///
    /// Default: `"(?i)AB#(\\d+)"` (case-insensitive). Group 1 must capture the numeric work-item ID;
    /// the full match is used for adapter-level reference detection (so the
    /// returned ref string includes any prefix like `AB#`).
    ///
    /// Examples for orgs that don't use `AB#N`:
    /// - `"\\B#(\\d{4,8})\\b"` — bare `#NNNNNN` (4–8 digit) work items.
    /// - `"(?i)\\bBUG\\s*#?(\\d+)\\b"` — `BUG 12345` / `BUG #12345`.
    ///
    /// Bare `#N` overlaps with GitHub PR/issue numbers; configure with care
    /// when both integrations are enabled.
    ///
    /// Validated at config-load time: the pattern must compile, and it must
    /// expose at least one capture group.
    #[serde(default = "default_ticket_regex")]
    pub ticket_regex: String,

    /// Team keys to filter (e.g. `["ENG", "PLATFORM"]`). Empty = all teams.
    #[serde(default)]
    pub team_keys: Vec<String>,

    /// Whether to fetch work items on commit reference (Phase 2+, currently ignored).
    #[serde(default = "default_true")]
    pub fetch_on_reference: bool,
}

fn default_ticket_regex() -> String {
    // `(?i)` makes the default case-insensitive so `ab#42` / `Ab#42` /
    // `AB#42` all match — preserves the behavior of the pre-issue-#74
    // hardcoded `(?i)\bAB#(\d+)\b` regex that this field replaces.
    "(?i)AB#(\\d+)".to_string()
}

fn default_true() -> bool {
    true
}

impl AzureDevOpsConfig {
    /// Validate the configuration.
    ///
    /// # Errors
    ///
    /// Returns [`TgaError::ConfigError`] when:
    /// - `organization_url` is empty
    /// - `organization_url` is an on-premises / TFS URL (only
    ///   `dev.azure.com` and `*.visualstudio.com` are accepted in Phase 1)
    /// - `pat` is empty or whitespace-only
    /// - `project` is empty
    /// - `ticket_regex` fails to compile or has no capture group 1
    pub fn validate(&self) -> Result<(), TgaError> {
        if self.organization_url.trim().is_empty() {
            return Err(TgaError::ConfigError(
                "pm.azure_devops.organization_url must not be empty".into(),
            ));
        }
        if !is_cloud_url(&self.organization_url) {
            return Err(TgaError::ConfigError(format!(
                "pm.azure_devops.organization_url {:?} is not an Azure DevOps cloud URL — \
                 on-premises ADO Server / TFS is not supported in Phase 1 \
                 (only dev.azure.com and *.visualstudio.com are accepted)",
                self.organization_url
            )));
        }
        if self.pat.trim().is_empty() {
            return Err(TgaError::ConfigError(
                "pm.azure_devops.pat must not be empty (Phase 1 uses PAT authentication)".into(),
            ));
        }
        if self.project.trim().is_empty() {
            return Err(TgaError::ConfigError(
                "pm.azure_devops.project must not be empty".into(),
            ));
        }
        // Compile the regex now so config-load surfaces a bad pattern early.
        let _ = self.compile_ticket_regex()?;
        Ok(())
    }

    /// Compile `ticket_regex` and verify the contract used by detection:
    /// the pattern must have at least one capture group, since group 1 is
    /// parsed as the numeric work-item ID for batch fetches.
    ///
    /// # Errors
    ///
    /// Returns [`TgaError::ConfigError`] when the pattern fails to compile
    /// or has no capture group 1.
    pub fn compile_ticket_regex(&self) -> Result<Regex, TgaError> {
        let re = Regex::new(&self.ticket_regex).map_err(|e| {
            TgaError::ConfigError(format!(
                "pm.azure_devops.ticket_regex {:?} failed to compile: {e}",
                self.ticket_regex
            ))
        })?;
        if re.captures_len() < 2 {
            return Err(TgaError::ConfigError(format!(
                "pm.azure_devops.ticket_regex {:?} has no capture group — \
                 group 1 must capture the numeric work-item ID \
                 (e.g. {:?})",
                self.ticket_regex,
                default_ticket_regex(),
            )));
        }
        Ok(re)
    }
}

/// Returns true if the URL is a valid Azure DevOps cloud URL.
///
/// Accepts only `dev.azure.com` and `*.visualstudio.com` host suffixes —
/// matches the Phase 1 ADR's "cloud only" decision.
fn is_cloud_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.contains("dev.azure.com") || lower.contains(".visualstudio.com")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(url: &str, pat: &str, project: &str) -> AzureDevOpsConfig {
        AzureDevOpsConfig {
            organization_url: url.to_string(),
            pat: pat.to_string(),
            project: project.to_string(),
            ticket_regex: default_ticket_regex(),
            team_keys: vec![],
            fetch_on_reference: true,
        }
    }

    #[test]
    fn cloud_url_accepted() {
        let c = cfg("https://dev.azure.com/myorg", "secret-pat", "MyProject");
        c.validate().expect("dev.azure.com URL should validate");
    }

    #[test]
    fn visualstudio_url_accepted() {
        let c = cfg("https://myorg.visualstudio.com", "secret-pat", "MyProject");
        c.validate().expect("visualstudio.com URL should validate");
    }

    #[test]
    fn on_prem_url_rejected() {
        let c = cfg("https://tfs.mycompany.com/tfs", "secret-pat", "MyProject");
        let err = c.validate().expect_err("on-prem URL must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("on-premises") || msg.contains("not an Azure DevOps cloud URL"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn empty_pat_rejected() {
        let c = cfg("https://dev.azure.com/myorg", "   ", "MyProject");
        let err = c.validate().expect_err("whitespace PAT must be rejected");
        assert!(format!("{err}").contains("pat"));
    }

    #[test]
    fn empty_url_rejected() {
        let c = cfg("", "secret", "MyProject");
        c.validate().expect_err("empty url must be rejected");
    }

    #[test]
    fn empty_project_rejected() {
        let c = cfg("https://dev.azure.com/myorg", "secret", "");
        c.validate().expect_err("empty project must be rejected");
    }

    #[test]
    fn default_ticket_regex_is_case_insensitive_ab_hash() {
        // Locked in by the pre-issue-#74 behavior. The old hardcoded
        // detection used `(?i)\bAB#(\d+)\b`; the default must keep that
        // case-insensitivity, or `ab#42` / `Ab#42` silently stop matching.
        assert_eq!(default_ticket_regex(), r"(?i)AB#(\d+)");
        let re = Regex::new(&default_ticket_regex()).expect("default compiles");
        assert!(re.is_match("AB#42"));
        assert!(re.is_match("ab#42"));
        assert!(re.is_match("Ab#42"));
    }

    #[test]
    fn yaml_deserialization() {
        let yaml = r#"
organization_url: "https://dev.azure.com/myorg"
pat: "secret-pat"
project: "MyProject"
team_keys: ["ENG", "PLATFORM"]
"#;
        let parsed: AzureDevOpsConfig =
            serde_yaml::from_str(yaml).expect("should deserialize cleanly");
        assert_eq!(parsed.organization_url, "https://dev.azure.com/myorg");
        assert_eq!(parsed.pat, "secret-pat");
        assert_eq!(parsed.project, "MyProject");
        assert_eq!(parsed.team_keys, vec!["ENG", "PLATFORM"]);
        // Defaults applied.
        assert_eq!(parsed.ticket_regex, r"(?i)AB#(\d+)");
        assert!(parsed.fetch_on_reference);
    }

    #[test]
    fn invalid_ticket_regex_rejected() {
        let mut c = cfg("https://dev.azure.com/myorg", "secret", "MyProject");
        c.ticket_regex = "(unclosed".to_string();
        let err = c.validate().expect_err("invalid regex must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("ticket_regex") && msg.contains("failed to compile"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn ticket_regex_without_capture_group_rejected() {
        let mut c = cfg("https://dev.azure.com/myorg", "secret", "MyProject");
        c.ticket_regex = r"AB#\d+".to_string();
        let err = c
            .validate()
            .expect_err("regex without capture group must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("ticket_regex") && msg.contains("no capture group"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn custom_ticket_regex_compiles() {
        let mut c = cfg("https://dev.azure.com/myorg", "secret", "MyProject");
        c.ticket_regex = r"\B#(\d{4,8})\b".to_string();
        let re = c
            .compile_ticket_regex()
            .expect("custom #NNNNNN regex should compile");
        let caps = re.captures("see #123080 and #99 here").expect("matches");
        assert_eq!(caps.get(0).unwrap().as_str(), "#123080");
        assert_eq!(caps.get(1).unwrap().as_str(), "123080");
    }

    #[test]
    fn yaml_deserialization_in_pm_block() {
        // Verifies the canonical YAML layout: pm.azure_devops.*
        let yaml = r#"
pm:
  azure_devops:
    organization_url: "https://myorg.visualstudio.com"
    pat: "x"
    project: "Demo"
"#;
        #[derive(Deserialize)]
        struct Wrap {
            pm: super::super::PmConfig,
        }
        let w: Wrap = serde_yaml::from_str(yaml).expect("pm.azure_devops should parse");
        let adc = w.pm.azure_devops.expect("azure_devops present");
        assert_eq!(adc.organization_url, "https://myorg.visualstudio.com");
        adc.validate().expect("should validate");
    }
}
