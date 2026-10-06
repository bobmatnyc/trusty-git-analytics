//! A skipped Jev call names its recovery command (#111 item 9).
//!
//! Why: a commit whose LLM call was skipped (spend cap or payload dump)
//! keeps its rule verdict and a `classification_id`, so a plain re-run never
//! revisits it; the operator needs the exact command that does.
//! No network: payload-dump mode sends nothing and reads no key.

use std::process::Command;

use tga::core::db::Database;

const BIN: &str = env!("CARGO_BIN_EXE_tga");

/// Why: see the module doc.
/// What: `tga classify` in dump mode over two unanswerable commits; stdout
/// must print the skipped count next to `classify --force --shas <file>`,
/// and that file must list both SHAs.
/// Test: this function.
#[test]
fn skipped_calls_print_the_recovery_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("tga.db");
    {
        let db = Database::open(&db_path).expect("create db");
        for (sha, msg) in [("sha-a", "zzz qqq"), ("sha-b", "vvv www")] {
            db.connection()
                .execute(
                    "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                     repository) VALUES (?1, 'a', 'a@x', '2024-01-01T00:00:00Z', ?2, 'r')",
                    rusqlite::params![sha, msg],
                )
                .expect("insert");
        }
    }
    let rules = dir.path().join("rules.yaml");
    std::fs::write(
        &rules,
        "extend_defaults: false\nrules:\n  - id: infra\n    category: platform\n    \
         keywords: [\"infra:\"]\n",
    )
    .expect("rules");
    let config = dir.path().join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "version: \"1.0\"\nrepositories: []\nclassification:\n  rules_files: [{}]\n  \
             weighted_sum:\n    enabled: false\nllm:\n  source: jev\n  jev:\n    \
             payload_dump_dir: {}\n",
            rules.display(),
            dir.path().join("dump").display()
        ),
    )
    .expect("config");

    let out = Command::new(BIN)
        .arg("--config")
        .arg(&config)
        .arg("--database")
        .arg(&db_path)
        .arg("classify")
        .env("TRUSTY_NO_UPDATE_CHECK", "1")
        .env_remove("TYPESAFE_API_KEY")
        .output()
        .expect("spawn tga");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "tga failed; stderr:\n{stderr}");
    assert!(stdout.contains("skipped 2"), "no skipped count:\n{stdout}");
    let line = stdout
        .lines()
        .find(|l| l.contains("classify --force --shas "))
        .unwrap_or_else(|| panic!("no recovery command:\n{stdout}"));
    let file = line
        .rsplit("--shas ")
        .next()
        .expect("path")
        .trim()
        .trim_matches('\'');
    let listed = std::fs::read_to_string(file).expect("sha file");
    assert_eq!(listed.lines().collect::<Vec<_>>(), ["sha-a", "sha-b"]);
}
