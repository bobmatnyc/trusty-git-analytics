//! #111: regression tests for the second gate-B inspection (on 92cbde0,
//! re-checked on d61bd42): commit hashes in prose, and a name matcher that
//! scales to a production-sized name set. No network; every name and hash
//! here is synthetic.

use std::time::{Duration, Instant};

use super::jev_obfuscate::{KnownNames, Obfuscator, NAME_MATCHER_SIZE_LIMIT};

/// A synthetic 40-character commit hash.
const SHA: &str = "3f9c2a7b1e5d4c8a9b0f6e2d1c3a5b7e9f0a2c4d";

/// A synthetic 64-character content hash.
const SHA256: &str = "9b1e0c4f7a2d5e8b3c6f9a0d1e4b7c2f5a8d0e3b6c9f2a5d8e1b4c7f0a3d6e9b";

fn names(repos: &[&str], people: &[&str]) -> KnownNames {
    let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
    KnownNames {
        repos: owned(repos),
        people: owned(people),
        ..KnownNames::default()
    }
}

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

/// Why (gate B 2, finding 1): a full hash inside a version string named a
/// real commit in the payload body.
#[test]
fn a_full_sha_in_a_version_string_becomes_id() {
    let out = run(&format!("bump the agent to 1.4.2-{SHA} and retry"));
    assert_absent(&out, &[SHA, &SHA[..7]]);
    assert!(out.contains("ID_1"), "{out}");
    assert!(out.starts_with("bump the agent to ") && out.ends_with(" and retry"));
}

/// Why (gate B 2, finding 1): `git describe` output carries an abbreviated
/// hash after `-g`; `@`, `+` and a bare word boundary carry them too.
#[test]
fn an_abbreviated_sha_after_g_and_other_joiners_becomes_id() {
    let short = &SHA[..7];
    let cases = [
        format!("built from v2.1.0-14-g{short} last night"),
        format!("pin tool@{short} for now"),
        format!("version 1.0.0+{short} is live"),
        format!("reverts {short}."),
        format!("reverts {}", SHA[..12].to_uppercase()),
    ];
    for case in cases {
        let out = run(&case);
        assert_absent(&out, &[short]);
        assert!(out.contains("ID_1"), "{out}");
    }
}

/// Why (gate B 2, finding 1): a hash in a URL path. The URL rule already
/// takes the whole URL, and a scheme-less slash path is a path; this
/// confirms neither leaves the hash.
#[test]
fn a_sha_in_a_url_path_is_not_sent() {
    for text in [
        format!("see https://git.example.test/acme/svc/commit/{SHA} for it"),
        format!("see git.example.test/acme/svc/commit/{SHA} for it"),
    ] {
        assert_absent(&run(&text), &[SHA, &SHA[..7]]);
    }
}

/// Why (gate B 2, finding 1): 32 or more hex characters are a hash or a
/// key whatever their mix, including a pure-digit run of that length.
#[test]
fn a_64_character_hash_becomes_id() {
    let digits = "1234567890".repeat(4);
    let out = run(&format!("artifact sha256:{SHA256} and {digits} verified"));
    assert_absent(&out, &[SHA256, &SHA256[..7], &digits]);
    assert_eq!(out, "artifact sha256:ID_1 and ID_2 verified");
}

/// Why (gate B 2, finding 1): a UUID is one id, never four visible parts.
#[test]
fn a_uuid_becomes_one_id() {
    let out = run("retry job 123e4567-e89b-42d3-a456-426614174000 now");
    assert_eq!(out, "retry job ID_1 now");
}

/// Why (gate B 2, finding 1): the hex rule needs a digit and a letter
/// below 32 characters, so words spelled in hex letters and plain numbers
/// pass through. What: each line is sent unchanged.
#[test]
fn hex_letter_words_and_plain_numbers_survive() {
    for text in [
        "a decade old facade: added the faced beef cafe deadbeef",
        "Decade, FACADE, Added; defaced effaced",
        "build 1234567 on 20261006, port 8080, issue 4815162342",
        "feedback0a1b2c3 and img3f9c2a7 and x3f9c2a7b stay",
    ] {
        assert_eq!(run(text), text);
    }
}

/// Why (gate B 2, finding 2): a known name inside a longer word is a
/// different word and must stay, in any case and next to `_` or a digit.
#[test]
fn a_name_inside_a_longer_word_is_not_replaced() {
    let mut o = Obfuscator::new(&names(&["acme"], &["jdoe"])).expect("builds");
    let text = "acmeish Bacme ACMEs acme_web acme2 jdoex xjdoe jdoe_ 2jdoe";
    assert_eq!(ob(&mut o, text), text);
    assert_eq!(ob(&mut o, "acme-web by JDoe."), "REPO_1-web by PERSON_1.");
}

/// Why (gate B 2, finding 2): when two known names overlap, the longest
/// one that is a whole word wins, across kinds and across whitespace.
#[test]
fn the_longest_of_two_overlapping_names_wins() {
    let mut o = Obfuscator::new(&names(&["acme", "acme-web-api"], &["Jane Doe"])).expect("builds");
    assert_eq!(ob(&mut o, "ship acme-web-api"), "ship REPO_1");
    assert_eq!(ob(&mut o, "ship acme-web-apis"), "ship REPO_2-web-apis");
    assert_eq!(ob(&mut o, "ask jane \t DOE"), "ask PERSON_1");
    assert_eq!(ob(&mut o, "ask Jane"), "ask PERSON_2");
}

/// Syllable spelling of `n`, `parts` syllables long: distinct `n` give
/// distinct words.
fn spell(mut n: usize, parts: usize) -> String {
    const SYL: [&str; 32] = [
        "qa", "ze", "vo", "xu", "ky", "jo", "wi", "pe", "ru", "ta", "no", "mi", "lu", "fe", "ga",
        "ho", "bi", "de", "si", "cu", "ra", "ve", "yo", "zi", "ok", "um", "is", "an", "el", "ob",
        "ur", "ix",
    ];
    let mut w = String::new();
    for _ in 0..parts {
        w.push_str(SYL[n % SYL.len()]);
        n /= SYL.len();
    }
    w
}

/// A synthetic name set shaped like a production database: file basenames
/// with extensions (most of it), logins, multi-word names and hyphenated
/// repository names. Returns the config names and the file paths.
fn production_shaped(paths: usize) -> (KnownNames, Vec<String>) {
    const EXT: [&str; 10] = [
        "rs", "py", "ts", "tsx", "md", "json", "go", "java", "yaml", "sql",
    ];
    let files = (0..paths)
        .map(|i| {
            let (a, b) = (spell(i, 3), spell(i / 7 + 11, 2));
            let base = match i % 3 {
                0 => format!("{a}_{b}"),
                1 => format!("{}{}", titled(&a), titled(&b)),
                _ => format!("{a}{}", i % 100),
            };
            format!("src/{}/{base}.{}", spell(i % 997, 2), EXT[i % EXT.len()])
        })
        .collect();
    let logins = (0..80_000).map(|i| format!("q{}{}", spell(i, 3), i % 89));
    let people =
        (0..40_000).map(|i| format!("{} {}", titled(&spell(i, 3)), titled(&spell(i * 7 + 5, 4))));
    let repos = (0..20_000)
        .map(|i| format!("org{}/{}-{}-svc", i % 40, spell(i, 2), spell(i * 3 + 1, 3)))
        .collect();
    let known = KnownNames {
        people: logins.chain(people).collect(),
        repos,
        ..KnownNames::default()
    };
    (known, files)
}

fn titled(w: &str) -> String {
    let mut c = w.chars();
    c.next()
        .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
        .unwrap_or_default()
}

/// The `names` and `matcher_bytes` counts from the obfuscator's `Debug`.
fn counts(o: &Obfuscator) -> (usize, usize) {
    let dbg = format!("{o:?}");
    let field = |key: &str| {
        dbg.split(&format!("{key}: "))
            .nth(1)
            .and_then(|rest| rest.split([',', ' ']).next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no {key} in {dbg}"))
    };
    (field("names"), field("matcher_bytes"))
}

/// Why (gate B 2, finding 2): the production database held 225,227 names,
/// the default 64 MiB cap refused them, and a debug build stalled for more
/// than 22 minutes building the matcher.
/// What: 230,000 file paths plus logins, people and repositories, over
/// 500,000 names in all, build at the default cap and still redact a
/// name, a file and a repository. The measured build times and sizes are
/// in `docs/requirements/configuration.md`.
#[test]
fn half_a_million_names_build_at_the_default_cap() {
    let (known, files) = production_shaped(230_000);
    let start = Instant::now();
    let mut o =
        Obfuscator::with_size_limit(&known, NAME_MATCHER_SIZE_LIMIT).expect("config names fit");
    o.add_files(&files).expect("file names fit the default cap");
    let built = start.elapsed();
    let (count, bytes) = counts(&o);
    eprintln!("{count} names: matcher {bytes} bytes, built in {built:?}");
    assert!(count >= 500_000, "only {count} names");
    assert!(bytes <= NAME_MATCHER_SIZE_LIMIT, "{bytes} bytes");
    assert!(built < Duration::from_secs(300), "took {built:?}");
    let base = files[123_457].rsplit('/').next().expect("basename");
    let (who, repo) = (known.people[100_123].clone(), known.repos[777].clone());
    let repo_name = repo.split('/').nth(1).expect("slug");
    let out = ob(&mut o, &format!("{who} fixed {base} in {repo_name} today"));
    assert_absent(&out, &[&who, base, repo_name]);
}
