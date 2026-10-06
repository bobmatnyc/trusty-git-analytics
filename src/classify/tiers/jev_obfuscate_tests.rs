//! #111: the Jev pseudonymizer, one test per span category plus a generated
//! property check. Pure functions; no network. All names and ids here are
//! synthetic.

use super::jev_obfuscate::{KnownNames, Obfuscator};

fn names(repos: &[&str], people: &[&str], terms: &[&str]) -> KnownNames {
    let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
    KnownNames {
        repos: owned(repos),
        people: owned(people),
        terms: owned(terms),
        ..KnownNames::default()
    }
}

fn obf(n: &KnownNames) -> Obfuscator {
    Obfuscator::new(n).expect("matcher builds")
}

fn ob(o: &mut Obfuscator, text: &str) -> String {
    o.obfuscate(text).expect("obfuscates").as_str().to_string()
}

/// `min + next(spread)` random lowercase letters.
fn word(next: &mut impl FnMut(u64) -> u64, min: u64, spread: u64) -> String {
    let len = min + next(spread);
    (0..len).map(|_| (b'a' + next(26) as u8) as char).collect()
}

fn run(input: &str) -> String {
    ob(&mut obf(&KnownNames::default()), input)
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

/// Why: every span category must map to its own pseudonym prefix; a file
/// keeps its extension (#111 item 12).
/// What: one input per category, with ordinary words left unchanged.
/// Test: this test.
#[test]
fn each_category_gets_its_pseudonym() {
    let cases = [
        ("ping jane.doe@corp.example.com today", "ping EMAIL_1 today"),
        (
            "fix ABC-123 and ABC-123, then XY9-7",
            "fix TICKET_1 and TICKET_1, then TICKET_2",
        ),
        ("see https://git.example.com/team/x/pull/4.", "see URL_1."),
        ("clone git@github.com:acme/app.git now", "clone URL_1 now"),
        (
            "point at db01.prod.internal and 10.2.3.4",
            "point at HOST_1 and HOST_2",
        ),
        (
            "edit src/billing/ledger.rs and /etc/app/conf",
            "edit PATH_1.rs and PATH_2",
        ),
        ("rename ledger_service.py", "rename PATH_1.py"),
        (
            "thanks @jdoe for the review",
            "thanks @PERSON_1 for the review",
        ),
    ];
    for (input, want) in cases {
        assert_eq!(run(input), want, "input: {input}");
    }
}

/// Why: standard names, code identifiers, timestamps, routes and ordinary
/// prose must reach the model unchanged (gate B, D and E).
#[test]
fn ordinary_text_passes_through() {
    for input in [
        "refactor: extract the parser module and add unit tests",
        "support UTF-8 and SHA-256 in the and/or read/write docs",
        "bump node.js to 20; max retries is 5",
        // #111 (gate B): `api/v2` is now a path; slash prose stays.
        "fix(api): handle v1.2.3 edge case in read/write mode",
        "call Class.method, read object.field and foo.bar() e.g. twice",
        "retry at 12:30:45 and 09:05:00 UTC",
        "route /accounts/{id} via std::io::Result",
        "keep utf-8, x86-64, sha-256, e2e-1, python-3.11, step-20, release-branch-22",
        "cite RFC3339 and ISO8601",
    ] {
        assert_eq!(run(input), input);
    }
}

/// Why: repository names, roster names and operator terms come from config;
/// a single-token roster name (`Max`) is a name too (#111 item 4).
/// What: each is replaced case-insensitively as a whole word.
#[test]
fn config_names_are_replaced() {
    let mut o = obf(&names(
        &["widgets-api", "acme"],
        &["Jane Doe", "Max", "jdoe42"],
        &["ledgerd"],
    ));
    let out = ob(
        &mut o,
        "bump Widgets-API for ACME; restart Ledgerd; ask Jane Doe or jdoe42; max 3",
    );
    assert_eq!(
        out,
        "bump REPO_1 for REPO_2; restart TERM_1; ask PERSON_1 or PERSON_2; PERSON_3 3"
    );
    // Not a whole word: left alone.
    assert_eq!(ob(&mut o, "ledgerdb"), "ledgerdb");
}

/// Why (#111 item 4): single-token logins and aliases are redacted, and
/// each token of a multi-word name is matched alone unless it is a
/// stop-list word.
#[test]
fn single_tokens_and_name_parts_are_redacted() {
    let mut o = obf(&names(
        &[],
        &["jdoe", "qmatson", "Max", "Jane Doe", "Will Quorra"],
        &[],
    ));
    let out = ob(
        &mut o,
        "jdoe paired with qmatson and Max; Jane and Doe agreed; Quorra will review",
    );
    assert_absent(&out, &["jdoe", "qmatson", "max", "jane", "doe", "quorra"]);
    assert!(out.contains(" will review"), "{out}");
}

/// Why (#111 item 3): a longer configured name that fails the whole-word
/// check must fall back to a shorter overlapping one.
#[test]
fn overlapping_names_fall_back_to_the_shorter() {
    let mut o = obf(&names(&["acme", "acme-web"], &[], &[]));
    assert_eq!(ob(&mut o, "ship acme-webhooks"), "ship REPO_1-webhooks");
    assert_eq!(ob(&mut o, "ship acme-web"), "ship REPO_2");
}

/// Why: git trailers name people verbatim (#113 redacts them for raters).
/// What: the trailer token stays; name → PERSON_n, address → EMAIL_n, and
/// the same name elsewhere in the message gets the same pseudonym.
#[test]
fn trailers_become_person_and_email() {
    let msg = "fix: parser, thanks Jane Doe\n\n\
               Co-authored-by: Jane Doe <jane@example.com>\n\
               Signed-off-by: Bob Roe <bob@example.org>\n\
               cc: team@example.net\n\
               Reviewed-by: Ann Lee";
    assert_eq!(
        run(msg),
        "fix: parser, thanks PERSON_1\n\n\
         Co-authored-by: PERSON_1 <EMAIL_1>\n\
         Signed-off-by: PERSON_2 <EMAIL_2>\n\
         cc: <EMAIL_3>\n\
         Reviewed-by: PERSON_3"
    );
}

/// Why (#111 item 1): any `*-by:` trailer and any trailer carrying an
/// address names someone, and the name is remembered for the run.
#[test]
fn every_by_trailer_and_email_trailer_is_identity() {
    let mut o = obf(&KnownNames::default());
    let first = ob(
        &mut o,
        "fix: cache\n\n\
         Approved-by: Ann Quill\n\
         Co-developed-by: Bo Vantz <bo@corp.test>\n\
         Suggested-by: Cy Dunmore\n\
         Requested-by: Di Evers\n\
         Reported-and-tested-by: Ed Foxley\n\
         Contact: Gil Hosk <gil@corp.test>",
    );
    let later = ob(
        &mut o,
        "thanks Ann Quill, Bo Vantz, Cy Dunmore, Di Evers, Ed Foxley and Gil Hosk",
    );
    let secrets = ["Ann Quill", "Vantz", "Dunmore", "Evers", "Foxley", "Hosk"];
    assert_absent(&first, &secrets);
    assert_absent(&later, &secrets);
    assert!(first.contains("Approved-by: PERSON_"), "{first}");
}

/// Why (gate B, A): Phabricator lines list usernames; every value is a
/// person remembered for the run, and `first.last` is never a host.
#[test]
fn phabricator_trailers_are_identity() {
    let mut o = obf(&KnownNames::default());
    let first = ob(
        &mut o,
        "Summary: speed up sync\n\
         Reviewers: qjdoe, qjane.roe, #qinfra-team\n\
         Reviewed By: qjane.roe\n\
         Subscribers: qmax.power\n\
         Auditors: qkim\n\
         Reported by: qlee.chan\n\
         Differential Revision: https://phab.example.test/D1234",
    );
    let later = ob(&mut o, "per qjane.roe and qkim, see qlee.chan notes");
    let secrets = [
        "qjdoe",
        "qjane",
        "qroe",
        "qinfra-team",
        "qmax",
        "qkim",
        "qlee",
        "phab.example.test",
        "D1234",
    ];
    assert_absent(&first, &secrets);
    assert_absent(&later, &secrets);
    assert!(!later.contains("HOST_"), "{later}");
    assert!(first.contains("Differential Revision: URL_"), "{first}");
}

/// Why (#111 item 6, gate D): any dotted name that can be a host is one,
/// IPv6 included, but code identifiers are not.
#[test]
fn dotted_names_are_hosts_unless_code_or_files() {
    let out =
        run("db7.prod, api.acme.io, acme-fin.corpnet, fe80::1ff:fe23:4567:890a and [2001:db8::1]");
    // IPv6 runs before the dotted pass, so it is numbered first.
    assert_eq!(out, "HOST_3, HOST_4, HOST_5, HOST_1 and [HOST_2]");
    assert_eq!(
        run("Class.method and object.field and foo.bar() at 12:30:45"),
        "Class.method and object.field and foo.bar() at 12:30:45"
    );
}

/// Why (#111 item 6, gate B): lowercase, mixed-case and bracketed ticket
/// keys leak as well as `ABC-123`; standard names do not.
#[test]
fn lowercase_ticket_rule() {
    assert_eq!(
        run("fix abc-123, [Xx-9999], xx-99999 and proj-42"),
        "fix TICKET_1, [TICKET_2], TICKET_3 and TICKET_4"
    );
    let keep = "utf-8 x86-64 sha-256 e2e-1 python-3.11 python-3 top-10 release-branch-22";
    assert_eq!(run(keep), keep);
}

/// Why (#111 item 6): branch names in merge subjects name people and
/// features; public branch names are kept.
#[test]
fn merge_subject_branches_are_replaced() {
    let out = run("Merge branch 'qdoe/fix-login' into 'main'\n\
         Merge pull request #12 from qacme-labs/feature-x\n\
         Merged in qteam/topic (pull request #3)\n\
         See merge request qgrp/qproj!7");
    assert_absent(
        &out,
        &[
            "qdoe",
            "fix-login",
            "qacme-labs",
            "feature-x",
            "qteam",
            "qgrp",
            "qproj",
        ],
    );
    assert!(out.contains("'BRANCH_1' into 'main'"), "{out}");
}

/// Why (#111 item 6): GitHub handles in any shape name people.
#[test]
fn handles_are_replaced() {
    let out = run("thanks @qjane_doe, @qorg/qteam-x, @qapp[bot] and @qjdoe.");
    assert_absent(&out, &["qjane", "qorg", "qteam", "qapp", "qjdoe"]);
}

/// Why (gate B, C): internal record ids leak; standard names stay.
#[test]
fn record_ids_become_id() {
    assert_eq!(
        run("order AB12345 and batch Q98765; see RFC3339"),
        "order ID_1 and batch ID_2; see RFC3339"
    );
}

/// Why (#111 item 12, gate B re-run): a file keeps its extension, and
/// well-known file names and `.github/workflows` paths are paths too.
#[test]
fn file_names_keep_extension_and_well_known_names_are_paths() {
    assert_eq!(
        run(
            "edit docs/plan.md, ci.yml, README.md, Dockerfile, package.json, Cargo.toml, \
             .github/workflows/release.yml and svc/billing/Dockerfile"
        ),
        "edit PATH_2.md, PATH_5.yml, PATH_6.md, PATH_1, PATH_7.json, PATH_8.toml, \
         PATH_3.yml and PATH_4"
    );
}

/// Why: pseudonyms must be stable for the run, so one person or ticket is
/// one token across commits.
#[test]
fn pseudonyms_are_stable_within_a_run() {
    let mut o = obf(&KnownNames::default());
    assert_eq!(ob(&mut o, "ABC-1 by x@example.com"), "TICKET_1 by EMAIL_1");
    assert_eq!(
        ob(&mut o, "X@EXAMPLE.COM closes ABC-2 and ABC-1"),
        "EMAIL_1 closes TICKET_2 and TICKET_1"
    );
}

/// Why: the leak guarantee must hold for inputs nobody wrote by hand.
/// What: 500 messages built by a fixed-seed generator from random e-mails,
/// URLs, ticket keys, roster names, hosts, paths, merge branches, handles
/// and trailers in random prose (#111 item 15); none may survive, and a
/// generated path keeps its extension.
/// Test: this test.
#[test]
fn no_generated_secret_survives() {
    let mut seed: u64 = 0x5eed_1111;
    let mut next = move |n: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % n
    };
    let prose = [
        "fix", "add", "the", "cache", "for", "in", "and", "see", "per",
    ];
    // Roster names: single tokens and two-word names, all synthetic.
    let roster: Vec<String> = (0..12)
        .map(|i| {
            let first = format!("q{}", word(&mut next, 5, 3));
            if i % 2 == 0 {
                first
            } else {
                format!("{first} z{}", word(&mut next, 5, 3))
            }
        })
        .collect();
    let trailer_pool: Vec<String> = (0..24)
        .map(|_| format!("K{} W{}", word(&mut next, 5, 3), word(&mut next, 5, 3)))
        .collect();
    let people: Vec<&str> = roster.iter().map(String::as_str).collect();
    let mut o = obf(&names(&[], &people, &[]));
    for _ in 0..500 {
        let mut secrets = Vec::new();
        let mut msg = String::new();
        let mut has_path = false;
        for _ in 0..(3 + next(6)) {
            let piece = match next(10) {
                0 => {
                    let e = format!(
                        "{}.{}@{}.{}",
                        word(&mut next, 3, 5),
                        word(&mut next, 2, 4),
                        word(&mut next, 3, 6),
                        ["com", "io", "org", "dev"][next(4) as usize]
                    );
                    secrets.push(e.clone());
                    e
                }
                1 => {
                    let u = format!(
                        "https://{}.example.com/{}/{}?q={}",
                        word(&mut next, 4, 1),
                        word(&mut next, 5, 1),
                        next(1000),
                        word(&mut next, 3, 1)
                    );
                    secrets.push(u.clone());
                    u
                }
                2 => {
                    let key = word(&mut next, 2, 3);
                    let key = if next(2) == 0 {
                        key.to_uppercase()
                    } else {
                        key
                    };
                    let t = format!("q{key}-{}", 10 + next(9990));
                    secrets.push(t.clone());
                    t
                }
                3 => {
                    let n = roster[next(roster.len() as u64) as usize].clone();
                    secrets.extend(n.split_whitespace().map(str::to_string));
                    n
                }
                4 => {
                    let h = format!(
                        "{}{}.{}.{}",
                        word(&mut next, 4, 3),
                        next(99),
                        word(&mut next, 4, 3),
                        ["internal", "corp", "io", "lan"][next(4) as usize]
                    );
                    secrets.push(h.clone());
                    h
                }
                5 => {
                    let (d, f) = (word(&mut next, 5, 3), word(&mut next, 5, 3));
                    secrets.push(d.clone());
                    secrets.push(f.clone());
                    has_path = true;
                    format!("src/{d}/{f}.rs")
                }
                6 => {
                    let b = format!("{}/{}", word(&mut next, 5, 3), word(&mut next, 5, 3));
                    secrets.push(b.clone());
                    format!("Merge branch '{b}' into 'main'")
                }
                7 => {
                    let h = format!("{}_{}", word(&mut next, 5, 3), word(&mut next, 3, 3));
                    secrets.push(h.clone());
                    format!("@{h}")
                }
                8 => {
                    // A recurring pool, as in real history: each new name
                    // rebuilds the matcher.
                    let n = trailer_pool[next(trailer_pool.len() as u64) as usize].clone();
                    secrets.extend(n.split_whitespace().map(str::to_string));
                    // The blank line ends the trailer block, so an indented
                    // piece after it is text, not a folded trailer value.
                    format!("\nAcked-and-reviewed-by: {n}\n\n")
                }
                _ => prose[next(prose.len() as u64) as usize].to_string(),
            };
            msg.push_str(&piece);
            msg.push_str([" ", ", ", ". ", "\n"][next(4) as usize]);
        }
        let out = ob(&mut o, &msg);
        let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
        assert_absent(&out, &refs);
        if has_path {
            assert!(out.contains(".rs"), "extension lost in {out:?}");
        }
    }
}

// ---- tests that only compile against the #111 v2 API ----

/// Why (#111): the built-in patterns compile, with no `.expect` behind them.
#[test]
fn patterns_compile() {
    assert!(super::jev_patterns::patterns().is_ok());
}

/// Why (#111 item 7): a matcher that cannot be built is a hard error,
/// never a run with names silently unmatched.
#[test]
fn matcher_build_error_is_an_error() {
    let n = names(&["acme"], &["Jane Doe", "jdoe"], &["ledgerd"]);
    let err = Obfuscator::with_size_limit(&n, 16)
        .expect_err("too small to build")
        .to_string();
    assert!(
        err.contains("names") && err.contains("nothing was sent"),
        "{err}"
    );
    assert!(!err.contains("jdoe") && !err.contains("ledgerd"), "{err}");
}

/// Why (#111 item 2): the run's authors come in after construction and
/// must be matched from then on, in full and by name part.
#[test]
fn added_people_are_matched() {
    let mut o = obf(&KnownNames::default());
    o.add_people(&["Odalys Ferrandine".into(), "oferrandine".into()])
        .expect("rebuild");
    let out = ob(&mut o, "ask Odalys or oferrandine; Ferrandine wrote it");
    assert_absent(&out, &["odalys", "ferrandine"]);
}

/// Why (gate B, C): operator id patterns run first; an invalid one is a
/// hard error.
#[test]
fn operator_id_patterns_become_id() {
    let mut n = KnownNames {
        id_patterns: vec![r"\bLOT-[0-9]{2}\b".into()],
        ..KnownNames::default()
    };
    let mut o = obf(&n);
    assert_eq!(ob(&mut o, "rebuild LOT-07 now"), "rebuild ID_1 now");
    n.id_patterns = vec!["(".into()];
    let err = Obfuscator::new(&n).expect_err("invalid regex").to_string();
    assert!(err.contains("id_patterns[0]"), "{err}");
}

/// Why: the dotted classifier is the host rule's single decision point.
#[test]
fn dotted_classifier_table() {
    use super::jev_patterns::{classify_dotted, Dotted};
    let cases = [
        ("db7.prod", None, Dotted::Host),
        ("acme.io", None, Dotted::Host),
        ("10.0.0.1", None, Dotted::Host),
        ("foo.bar", Some('('), Dotted::Keep),
        ("Class.method", None, Dotted::Keep),
        ("object.field", None, Dotted::Keep),
        ("v1.2.3", None, Dotted::Keep),
        ("plan.md", None, Dotted::File("md")),
        ("node.js", None, Dotted::Keep),
        // #111 (gate B re-run): well-known file names are paths too.
        ("Cargo.toml", None, Dotted::File("toml")),
    ];
    for (s, next, want) in cases {
        assert_eq!(classify_dotted(s, next), want, "{s}");
    }
}

/// Why (#111, fail closed): after a failed matcher rebuild the name set holds
/// names the matcher lacks, so a later message naming one would pass it
/// through; every later call must fail instead.
/// What: a cap that fits the empty matcher but not a 300-name trailer; the
/// trailer message fails, then a message naming one of those people (in
/// prose and an already-seen trailer) fails too, as does an unrelated one.
#[test]
fn failed_rebuild_poisons_the_run() {
    let limit = small_limit();
    let mut o = Obfuscator::with_size_limit(&KnownNames::default(), limit).expect("empty fits");
    let big = big_trailer();
    assert!(
        o.obfuscate(&big).is_err(),
        "300 names must not fit in {limit} bytes"
    );
    let again = "thanks qzed0001 for the fix\n\nSubscribers: qzed0001";
    assert!(
        o.obfuscate(again).is_err(),
        "a seen name must not pass after a failed rebuild"
    );
    assert!(o.obfuscate("plain text").is_err());
}

/// A matcher size cap that one short name fits and 300 names do not.
pub(super) fn small_limit() -> usize {
    let one = KnownNames {
        people: vec!["qaa".into()],
        ..KnownNames::default()
    };
    // #111 (gate B 2): steps of 1/8, since an automaton's fixed overhead
    // keeps one name and 300 names within a factor of two.
    let limit = std::iter::successors(Some(256_usize), |l| Some(l + l / 8))
        .take_while(|&l| l < 1 << 24)
        .find(|&l| Obfuscator::with_size_limit(&one, l).is_ok())
        .expect("some cap fits one name");
    let many = KnownNames {
        people: (0..300).map(|i| format!("qzed{i:04}")).collect(),
        ..KnownNames::default()
    };
    assert!(Obfuscator::with_size_limit(&many, limit).is_err());
    limit
}

/// A Phabricator trailer naming 300 synthetic users.
pub(super) fn big_trailer() -> String {
    let names: Vec<String> = (0..300).map(|i| format!("qzed{i:04}")).collect();
    format!("fix cache\n\nSubscribers: {}", names.join(", "))
}
