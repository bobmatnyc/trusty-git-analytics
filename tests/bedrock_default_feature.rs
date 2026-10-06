//! `llm.source: bedrock` works in a default build.
//!
//! Why: the owner ruled that tga supports Bedrock, OpenRouter, Anthropic API
//! and Jev through user configuration alone. Before `bedrock` became a default
//! feature, a plain `cargo install tga` rejected `llm.source: bedrock` with
//! "bedrock feature not compiled in".
//! What: checks that the manifest's default feature set names `bedrock`, that
//! a config selecting Bedrock builds a Bedrock classifier when the feature is
//! on, and that a `--no-default-features` build still returns the rebuild
//! error. No AWS call is made: the Bedrock client is built on first use.
//! Test: this file IS the test. The gate's `no-default-features` step runs the
//! `cfg(not(feature = "bedrock"))` test.

use std::path::Path;

use tga::classify::tiers::bedrock::DEFAULT_BEDROCK_MODEL;
use tga::classify::tiers::llm::LlmClassifier;
use tga::core::config::{Config, LlmSource};

/// A config that selects Bedrock and nothing else LLM-related.
const BEDROCK_CONFIG: &str = r#"
repositories:
  - name: app
    path: /src/app
llm:
  source: bedrock
  region: us-east-1
"#;

/// Load [`BEDROCK_CONFIG`] through the public loader and build its classifier.
async fn bedrock_classifier() -> Result<LlmClassifier, String> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tga.yaml");
    std::fs::write(&path, BEDROCK_CONFIG).expect("write config");
    let cfg = Config::load(&path).expect("config loads");
    let llm = cfg.llm.expect("llm section");
    assert_eq!(llm.source, LlmSource::Bedrock);
    LlmClassifier::from_llm_config(&llm, DEFAULT_BEDROCK_MODEL).await
}

/// Why: `cargo install tga` and the release workflow build default features
/// only, so Bedrock reaches users only if `default` names it.
/// What: parses this crate's Cargo.toml and asserts `features.default`
/// contains `bedrock` and that the `bedrock` feature still exists, so
/// `--features bedrock` keeps working.
/// Test: this test.
#[test]
fn default_features_include_bedrock() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).expect("read Cargo.toml");
    let doc: toml::Table = toml::from_str(&text).expect("parse Cargo.toml");
    let features = doc["features"].as_table().expect("[features] table");
    let default: Vec<&str> = features["default"]
        .as_array()
        .expect("default is an array")
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    assert!(
        default.contains(&"bedrock"),
        "default features must include bedrock, got {default:?}"
    );
    assert!(
        features.contains_key("bedrock"),
        "the bedrock feature must stay"
    );
}

/// Why: a config naming `llm.source: bedrock` must build a working Bedrock
/// classifier in a default build, not raise the missing-feature error.
/// What: builds the classifier from YAML and asserts it routes to Bedrock.
/// Test: this test.
#[cfg(feature = "bedrock")]
#[tokio::test]
async fn bedrock_source_builds_a_bedrock_classifier() {
    let llm = match bedrock_classifier().await {
        Ok(llm) => llm,
        Err(e) => panic!("llm.source: bedrock must build in a default build: {e}"),
    };
    assert_eq!(llm.provider_label(), "bedrock");
    assert!(llm.has_api_key(), "a Bedrock backend counts as configured");
    assert_eq!(llm.model(), DEFAULT_BEDROCK_MODEL);
}

/// Why: `--no-default-features` drops the AWS SDK; selecting Bedrock there
/// must fail loudly with rebuild guidance, not silently skip the LLM tier.
/// What: builds the classifier from YAML and asserts the exact error.
/// Test: this test, run by the gate's `no-default-features` step.
#[cfg(not(feature = "bedrock"))]
#[tokio::test]
async fn bedrock_source_errors_without_the_feature() {
    use tga::classify::tiers::bedrock::BEDROCK_NOT_BUILT;

    let err = match bedrock_classifier().await {
        Ok(_) => panic!("a build without the bedrock feature must reject llm.source: bedrock"),
        Err(e) => e,
    };
    assert_eq!(err, BEDROCK_NOT_BUILT);
}
