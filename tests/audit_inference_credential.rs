//! #210: `tga audit`'s inference-credential check follows the provider the
//! `TRUSTY_REVIEW_*` environment selects.
//!
//! Runs the real `tga` binary, so the check is exercised through `main`'s
//! dispatch exactly as the `taudit audit` child runs it. The environment is set
//! on the child process only; this test process's environment is never
//! mutated. No network: the renderer is a stub that reports a pre-0.15
//! version, so a run that gets past the credential check stops at the very
//! next preflight (`ReviewBinaryTooOld`) before any daemon or model call.

#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output, Stdio};

/// The four variables `trusty-audit` sets on its `tga audit` child.
const SELECTION: [&str; 4] = [
    "TRUSTY_REVIEW_PROVIDER",
    "TRUSTY_REVIEW_REVIEWER_MODEL",
    "TRUSTY_REVIEW_VERIFIER_MODEL",
    "TRUSTY_REVIEW_SUMMARIZER_MODEL",
];

/// The missing-key refusal's opening words.
const NO_KEY: &str = "OPENROUTER_API_KEY is not set";

/// A `trusty-review` stand-in that answers `--version` with a version below
/// tga's floor.
///
/// Written by a `cat` child rather than `std::fs::write`, so this process never
/// holds a write descriptor on the file that a sibling test's fork could copy
/// and turn into `ETXTBSY` (the same reason as `trusty-audit`'s
/// `stub_executable`, #152).
fn old_renderer(dir: &Path) -> std::path::PathBuf {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join("trusty-review");
    let mut writer = Command::new("/bin/sh")
        .arg("-c")
        .arg("exec /bin/cat > \"$1\"")
        .arg("sh")
        .arg(&path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn the stub writer");
    writer
        .stdin
        .take()
        .expect("stub writer stdin")
        .write_all(b"#!/bin/sh\necho 'trusty-review 0.1.0'\n")
        .expect("write the stub");
    assert!(writer.wait().expect("wait for the stub writer").success());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make the stub executable");
    path
}

/// Run `tga audit` in `dir` with no OpenRouter key and `provider` selected
/// through all four `TRUSTY_REVIEW_*` variables.
fn audit_without_a_key(dir: &Path, provider: &str, models: [&str; 3]) -> Output {
    let values = [provider, models[0], models[1], models[2]];
    let mut command = Command::new(env!("CARGO_BIN_EXE_tga"));
    command
        .current_dir(dir)
        .env("TRUSTY_NO_UPDATE_CHECK", "1")
        .env("TRUSTY_REVIEW_BIN", old_renderer(dir))
        .env_remove("OPENROUTER_API_KEY")
        .args(["--config", "absent-config.yaml", "--database"])
        .arg(dir.join("tga.db"))
        .args(["audit", "--output"])
        .arg(dir.join("out"));
    for (name, value) in SELECTION.into_iter().zip(values) {
        command.env(name, value);
    }
    command.output().expect("run tga")
}

/// 🔴 #210, the Architect's required test: `taudit audit` under
/// `provider = "bedrock"` hands its `tga audit` child the four Bedrock
/// `TRUSTY_REVIEW_*` variables and no OpenRouter key. The child must get past
/// its credential check. Before the fix it refused with "OPENROUTER_API_KEY is
/// not set" before any Bedrock call.
#[test]
fn a_bedrock_audit_with_no_openrouter_key_gets_past_the_credential_check() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = audit_without_a_key(
        tmp.path(),
        "bedrock",
        [
            "us.anthropic.claude-sonnet-4-6",
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!stderr.contains(NO_KEY), "refused on the key: {stderr}");
    // Stopped by the next preflight, which proves the credential check passed.
    assert!(
        stderr.contains("reports version 0.1.0"),
        "expected the renderer-version refusal: {stderr}"
    );
    assert!(!out.status.success());
}

/// #210 error arm: an openrouter selection with no key is refused exactly as
/// before, with the same message.
#[test]
fn an_openrouter_audit_with_no_key_is_still_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = audit_without_a_key(
        tmp.path(),
        "openrouter",
        [
            "anthropic/claude-opus-4.8",
            "anthropic/claude-haiku-4.5",
            "anthropic/claude-haiku-4.5",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success());
    assert!(stderr.contains(NO_KEY), "{stderr}");
    assert!(stderr.contains("export OPENROUTER_API_KEY="), "{stderr}");
}
