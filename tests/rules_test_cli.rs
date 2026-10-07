//! #165: `tga rules test` classifies with the engine `tga classify` builds.
//! Runs the `tga` binary; no network.

use std::fs;
use std::path::Path;
use std::process::Command;

const MSG: &str = "chore(deps): bump openssl for CVE-2026-1234";

/// A rules file whose one rule never matches. Its category is `chore`, so
/// the weighted-sum verdict for [`MSG`] is inside the active category set
/// and only `weighted_sum.enabled` decides whether the tier fires.
fn rules(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("rules.yaml");
    fs::write(
        &path,
        "extend_defaults: false\nrules:\n  - id: never\n    category: chore\n    \
         keywords: [\"zzz-never-matches-zzz\"]\n",
    )
    .expect("rules");
    path
}

/// `tga rules test MSG` under a config naming `rules` plus `extra` under
/// `classification:`; returns stdout.
fn rules_test(dir: &Path, rules: &Path, extra: &str) -> String {
    let cfg = dir.join("config.yaml");
    fs::write(
        &cfg,
        format!(
            "classification:\n  rules_files: [\"{}\"]\n{extra}",
            rules.display()
        ),
    )
    .expect("config");
    let out = Command::new(env!("CARGO_BIN_EXE_tga"))
        .current_dir(dir)
        .args([
            "--config",
            cfg.to_str().expect("utf-8"),
            "rules",
            "test",
            MSG,
        ])
        .output()
        .expect("run tga");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "tga failed: {stderr}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Why (#165 defect 1): `rules test` built a default engine config, so
/// `weighted_sum.enabled: false` still reported a `weighted_sum` verdict
/// that `tga classify` never writes.
/// What: with the tier off, the dependency bump gets no verdict. The control
/// run, tier on, gets `weighted_sum` / `chore`, which proves the fixture
/// reaches the tier.
/// Test: this test.
#[test]
fn rules_test_honours_weighted_sum_enabled_false() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rules = rules(dir.path());

    let on = rules_test(dir.path(), &rules, "");
    assert!(on.contains("method      : weighted_sum"), "control: {on}");
    assert!(on.contains("category    : chore"), "control: {on}");

    let off = rules_test(dir.path(), &rules, "  weighted_sum:\n    enabled: false\n");
    assert!(!off.contains("weighted_sum"), "tier is off: {off}");
    assert!(off.contains("No tier matched"), "tier is off: {off}");
}
