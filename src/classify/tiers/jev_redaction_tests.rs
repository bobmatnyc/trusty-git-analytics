//! #111: one test per redaction category of the Jev payload, over realistic
//! commit-message shapes, plus the false-positive guards. Pure functions;
//! no network. Every name, host and id here is synthetic.

use super::jev_obfuscate::{KnownNames, Obfuscator};
use super::jev_patterns::{classify_dotted, Dotted};

fn ob(o: &mut Obfuscator, text: &str) -> String {
    o.obfuscate(text).expect("obfuscates").as_str().to_string()
}

fn run(text: &str) -> String {
    ob(
        &mut Obfuscator::new(&KnownNames::default()).expect("builds"),
        text,
    )
}

fn assert_absent(out: &str, secrets: &[&str]) {
    let lower = out.to_lowercase();
    for s in secrets {
        assert!(
            !lower.contains(&s.to_lowercase()),
            "{s:?} survived in {out:?}"
        );
    }
}

/// Why: git trailers name the authors and reviewers verbatim, and the same
/// people appear in prose.
#[test]
fn git_trailers_hide_names_and_emails() {
    let mut o = Obfuscator::new(&KnownNames::default()).expect("builds");
    let out = ob(
        &mut o,
        "fix(auth): refresh token on 401\n\n\
         Refresh was skipped when Qarlo retried.\n\n\
         Co-authored-by: Qarlo Venn <qarlo.venn@corp.test>\n\
         Signed-off-by: Mira Tolsk <mtolsk@corp.test>\n\
         Reviewed-by: Dex Obrin <dex@corp.test>",
    );
    let later = ob(&mut o, "follow-up for Tolsk and Obrin, cc mtolsk");
    let secrets = [
        "qarlo",
        "venn",
        "mira",
        "tolsk",
        "obrin",
        "corp.test",
        "mtolsk",
    ];
    assert_absent(&out, &secrets);
    assert_absent(&later, &secrets);
    assert!(out.contains("Co-authored-by: PERSON_2 <EMAIL_1>"), "{out}");
    assert!(out.contains("Reviewed-by: PERSON_"), "{out}");
}

/// Why (gate B): Phabricator bodies list usernames under `Reviewers:`,
/// `Reviewed By:` and `Subscribers:` — comma- or space-separated, with a
/// blocking reviewer's `!` and `#project` tags — and the stress run leaked
/// them.
#[test]
fn phabricator_username_shapes() {
    let mut o = Obfuscator::new(&KnownNames::default()).expect("builds");
    let out = ob(
        &mut o,
        "[sync] speed up nightly import\n\n\
         Summary: batch writes; per qvoss the lock is fine.\n\n\
         Test Plan: ran the import twice\n\n\
         Reviewers: qvoss, qlinde!, #qplatform\n\n\
         Reviewed By: qvoss\n\n\
         Subscribers: qharp qmeyer qtan, qosei\n\n\
         Differential Revision: https://phab.corp.test/D4821",
    );
    let later = ob(
        &mut o,
        "thanks qlinde and qmeyer; qtan to follow up with qosei",
    );
    let secrets = [
        "qvoss",
        "qlinde",
        "qplatform",
        "qharp",
        "qmeyer",
        "qtan",
        "qosei",
        "phab.corp",
        "D4821",
    ];
    assert_absent(&out, &secrets);
    assert_absent(&later, &secrets);
    assert!(
        out.contains("Reviewers: PERSON_1, PERSON_2, #PERSON_3"),
        "{out}"
    );
}

/// Why (gate B): squash bodies bullet or quote the trailers, git folds a
/// long value onto an indented line, and a value can read
/// `login (Real Name)`; every shape names people.
#[test]
fn folded_and_bulleted_trailers() {
    let mut o = Obfuscator::new(&KnownNames::default()).expect("builds");
    let out = ob(
        &mut o,
        "Squashed commits:\n\
         * Reviewers: qbrand\n\
         * Reviewed By: qbrand\n\
         > Subscribers: qnoor\n\
         Reviewed-by: qkell (Qira Kellan)\n\
         Signed-off-by: Pell Arno <parno@corp.test>,\n    \
         Qin Mavro <qmavro@corp.test>",
    );
    let later = ob(&mut o, "thanks qbrand, qnoor, qkell, Kellan and Mavro");
    let secrets = [
        "qbrand",
        "qnoor",
        "qkell",
        "qira",
        "kellan",
        "arno",
        "mavro",
        "corp.test",
    ];
    assert_absent(&out, &secrets);
    assert_absent(&later, &secrets);
    assert!(out.contains("* Reviewers: PERSON_"), "{out}");
    assert!(
        out.contains("\n    PERSON_"),
        "continuation kept its indent: {out}"
    );
}

/// Why: ticket keys leak in any case and in brackets; `[abc-123]` and
/// `ABC-123` are one ticket.
#[test]
fn ticket_keys_any_case_and_bracketed() {
    assert_eq!(run("[abc-123] fix ABC-123"), "[TICKET_1] fix TICKET_1");
    let out = run("[Abc-7] and [xyz-1]: closes Proj-77, PROJ-78, MY_PROJ-9 and build_QAB-12");
    assert_absent(
        &out,
        &["abc-7", "xyz-1", "proj-77", "proj-78", "my_proj", "qab-12"],
    );
    assert!(out.contains("build_TICKET_"), "{out}");
    let keep = "utf-8 python-3.11 step-2 x86-64";
    assert_eq!(run(keep), keep);
}

/// Why: internal record ids (one letter and four digits, or an operator
/// pattern) identify customers and records.
#[test]
fn record_ids_one_letter_four_digits_and_operator_patterns() {
    assert_eq!(
        run("price fix for store H1234 and item P98765; see RFC3339"),
        "price fix for store ID_1 and item ID_2; see RFC3339"
    );
    let names = KnownNames {
        id_patterns: vec![r"\bORD-[0-9]{6}\b".into()],
        ..KnownNames::default()
    };
    let mut o = Obfuscator::new(&names).expect("builds");
    assert_eq!(ob(&mut o, "replay ORD-004211 now"), "replay ID_1 now");
}

/// Why: URLs, host names, IPv4 and file paths name infrastructure.
#[test]
fn urls_hosts_ips_and_paths() {
    let out = run(
        "deploy https://grafana.corp.test/d/qx1?from=now to db7.qacme.internal and \
         10.20.30.40; edit services/qbilling/ledger.rs and C:\\qdata\\conf.ini",
    );
    assert_absent(
        &out,
        &[
            "grafana",
            "db7",
            "qacme",
            "10.20.30.40",
            "qbilling",
            "ledger",
            "qdata",
        ],
    );
    assert!(out.contains("PATH_1.rs") && out.contains(".ini"), "{out}");
}

/// Why: IPv6 addresses are hosts, but Rust paths such as `f64::MAX`, key
/// fingerprints and times look like them.
#[test]
fn ipv6_but_not_lookalikes() {
    let out = run("listen on ::1, [2001:db8::42]:8080 and fe80::1%en0");
    assert_absent(&out, &["2001:db8", "fe80"]);
    assert_eq!(out, "listen on HOST_1, [HOST_2]:8080 and HOST_3%en0");
    let keep =
        "use f64::MAX and f32::EPSILON; key 16:27:ac:a5:76:28:2d:36:63:1b:56:4d:eb:df:a6:48 \
                at 12:30:45";
    assert_eq!(run(keep), keep);
}

/// Why: the `dev`, `app` and `local` suffixes made ordinary code words
/// hosts; real hosts on those suffixes must still be caught.
#[test]
fn hosts_but_not_dev_app_local_words() {
    let keep = "load config.local and .env.local; set window.app and process.env.dev; \
                e2e.test passes; this.app.local";
    assert_eq!(run(keep), keep);
    let out = run("point db7.local, api.qacme.dev and my-svc.app at qacme.io");
    assert_absent(&out, &["db7", "qacme", "my-svc"]);
    let cases = [
        ("config.local", Dotted::Keep),
        ("window.app", Dotted::Keep),
        ("process.env.dev", Dotted::Keep),
        // #111 (gate B re-run): no two-label suffix exemption is left.
        ("go.to", Dotted::Host),
        ("db7.local", Dotted::Host),
        ("api.qacme.dev", Dotted::Host),
        ("my-svc.app", Dotted::Host),
    ];
    for (s, want) in cases {
        assert_eq!(classify_dotted(s, None), want, "{s}");
    }
}

/// Why: configured repository and service names are internal.
#[test]
fn repo_and_service_names_where_configured() {
    let names = KnownNames {
        repos: vec!["qledger-core".into()],
        terms: vec!["qpaygate".into()],
        ..KnownNames::default()
    };
    let mut o = Obfuscator::new(&names).expect("builds");
    assert_eq!(
        ob(&mut o, "bump QLedger-Core client for qpaygate retries"),
        "bump REPO_1 client for TERM_1 retries"
    );
}
