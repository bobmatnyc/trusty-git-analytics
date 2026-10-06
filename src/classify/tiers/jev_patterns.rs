//! Detection rules behind the Jev pseudonymizer (#111).
//!
//! Why: [`super::jev_obfuscate`] owns the pseudonym map and the pass order;
//! the patterns, lists and predicates that decide what a sensitive span is
//! live here, so each rule has one home and both files stay under the size
//! cap. Trailer parsing lives in [`super::jev_trailers`].
//! What: the regexes each pass runs ([`Patterns`]), the dotted-name
//! classifier (host / file / keep), the ticket-key and IPv6 rules, the name
//! and ticket stop-lists, and the name-matcher regex builder.
//! Test: `classify::tiers::jev_obfuscate_tests`,
//! `classify::tiers::jev_redaction_tests`.

use std::net::Ipv6Addr;
use std::sync::LazyLock;

use regex::Regex;

use super::jev_error::JevError;

/// Every built-in detection regex, compiled once per process.
///
/// Why (#111): a pattern that fails to compile must stop the run before
/// anything is sent, never panic; library code carries no `.expect`.
/// What: one field per pass; [`patterns`] hands out the compiled set or
/// [`JevError::Pattern`] naming the pattern that failed.
/// Test: `jev_obfuscate_tests::patterns_compile`.
pub(super) struct Patterns {
    /// `scheme://…`, `git@host:path` and `www.…` URLs.
    pub(super) url: Regex,
    /// `local@domain.tld` addresses.
    pub(super) email: Regex,
    /// Slash- or backslash-separated tokens; [`is_path`] decides.
    pub(super) slash: Regex,
    /// Dotted names (`label.label[.label…]`); [`classify_dotted`] decides.
    pub(super) dotted: Regex,
    /// IPv6 candidates (group 2) after a non-address character (group 1);
    /// [`is_ipv6`] decides.
    pub(super) ipv6: Regex,
    /// `ABC-123`, `abc-123` and `MY_PROJ-7` issue-key candidates;
    /// [`super::jev_tickets::ticket_start`] decides.
    pub(super) ticket: Regex,
    /// Record ids: 1–4 letters then 4+ digits (`H1234`, `AB12345`);
    /// [`super::jev_tickets::is_record_id`] decides.
    pub(super) id: Regex,
    /// `@handle`, `@org/team` and `@app[bot]` mentions not preceded by a
    /// word character, `.`, `@` or `/`.
    pub(super) mention: Regex,
    /// A git merge subject naming branches in quotes (`Merge branch 'x/y'`).
    pub(super) merge_branch: Regex,
    /// A quoted span on a merge-subject line (group 2).
    pub(super) quoted: Regex,
    /// The source repository of `Merge branch 'x' of <source>` (group 2).
    pub(super) merge_of: Regex,
    /// `Merge pull request #N from owner/branch` (group 2).
    pub(super) merge_pr: Regex,
    /// Bitbucket's `Merged in x/y` (group 2).
    pub(super) merged_in: Regex,
    /// GitLab's `See merge request group/project!12` (group 2).
    pub(super) merge_request: Regex,
    /// A pseudonym this module emits (`PERSON_3`), for the dump token map.
    pub(super) placeholder: Regex,
}

impl Patterns {
    fn compile() -> Result<Self, &'static str> {
        let c = |name: &'static str, src: &str| Regex::new(src).map_err(|_| name);
        Ok(Self {
            url: c(
                "url",
                r#"(?i)\b(?:[a-z][a-z0-9+.-]*://[^\s<>"'`]+|git@[^\s:/]+:[^\s<>"'`]+|www\.[^\s<>"'`]+)"#,
            )?,
            email: c(
                "email",
                r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+",
            )?,
            slash: c(
                "slash",
                r"(?:[A-Za-z]:\\|~/|\.\.?/|/)?[A-Za-z0-9_.@+-]+(?:[/\\][A-Za-z0-9_.@+-]+)+[/\\]?",
            )?,
            dotted: c(
                "dotted",
                r"\b[A-Za-z0-9_][A-Za-z0-9_-]*(?:\.[A-Za-z0-9_-]+)+",
            )?,
            ipv6: c(
                "ipv6",
                r"(?i)(^|[^0-9a-z_:.])((?:[0-9a-f]{0,4}:){2,7}(?:[0-9a-f]{1,4}|\d{1,3}(?:\.\d{1,3}){3})?)",
            )?,
            // #111: `_` is legal in a Jira project key (`MY_PROJ-7`).
            // #111: no trailing `\b`, so `PROJ-1234_fix` matches; `is_ticket`
            // rejects a key run on into a letter.
            ticket: c("ticket", r"\b([A-Za-z][A-Za-z0-9_]{1,19})-([0-9]+)")?,
            id: c("id", r"\b([A-Za-z]{1,4})[0-9]{4,}\b")?,
            mention: c(
                "mention",
                r"(^|[^A-Za-z0-9_.@/])@([A-Za-z0-9_](?:[A-Za-z0-9_.-]*[A-Za-z0-9_])?(?:/[A-Za-z0-9_](?:[A-Za-z0-9_.-]*[A-Za-z0-9_])?)?(?:\[bot\])?)",
            )?,
            merge_branch: c(
                "merge_branch",
                r"(?i)\bmerge (?:remote-tracking )?branch(?:es)? ",
            )?,
            quoted: c("quoted", r#"(['"])([^'"\n]+)(['"])"#)?,
            merge_of: c(
                "merge_of",
                r#"(?i)(\bmerge (?:remote-tracking )?branch(?:es)? .*?['"] of )(\S+)"#,
            )?,
            merge_pr: c("merge_pr", r"(?i)(\bmerge pull request #\d+ from )(\S+)")?,
            merged_in: c("merged_in", r"(?i)(\bmerged in )(\S+)")?,
            merge_request: c("merge_request", r"(?i)(\bmerge request )([^\s!]+)(!\d+)")?,
            placeholder: c(
                "placeholder",
                r"\b(?:EMAIL|PERSON|TICKET|URL|HOST|PATH|BRANCH|REPO|TERM|ID)_[0-9]+\b",
            )?,
        })
    }
}

static PATTERNS: LazyLock<Result<Patterns, &'static str>> = LazyLock::new(Patterns::compile);

/// The compiled detection patterns.
///
/// # Errors
///
/// [`JevError::Pattern`] when a built-in pattern does not compile.
pub(super) fn patterns() -> Result<&'static Patterns, JevError> {
    PATTERNS.as_ref().map_err(|name| JevError::Pattern(name))
}

/// Framework names shaped like file names (`node.js`); kept.
const NOT_FILES: &[&str] = &[
    "node.js",
    "vue.js",
    "next.js",
    "nuxt.js",
    "react.js",
    "three.js",
    "d3.js",
    "chart.js",
    "express.js",
    "ember.js",
    "backbone.js",
];

/// File extensions: a dotted name ending in one is a file (`PATH_n.<ext>`),
/// never a host, and a pseudonymized path keeps it (#111).
pub(super) const FILE_EXTENSIONS: &[&str] = &[
    "bash",
    "bat",
    "c",
    "cc",
    "cfg",
    "cjs",
    "conf",
    "cpp",
    "cs",
    "css",
    "csv",
    "cxx",
    "dart",
    "dockerfile",
    "env",
    "ex",
    "exs",
    "gif",
    "go",
    "gradle",
    "gz",
    "h",
    "hpp",
    "htm",
    "html",
    "ini",
    "ipynb",
    "java",
    "jpeg",
    "jpg",
    "js",
    "json",
    "jsx",
    "kt",
    "kts",
    "lock",
    "log",
    "lua",
    "md",
    "mjs",
    "pdf",
    "php",
    "pl",
    "png",
    "proto",
    "ps1",
    "py",
    "r",
    "rb",
    "rs",
    "rst",
    "sass",
    "scala",
    "scss",
    "sh",
    "sql",
    "svelte",
    "svg",
    "swift",
    "tar",
    "tf",
    "tfvars",
    "toml",
    "ts",
    "tsx",
    "txt",
    "vue",
    "wasm",
    "xml",
    "yaml",
    "yml",
    "zip",
    "zsh",
    // #111 (gate B): more source, config, data and doc extensions.
    "adoc",
    "avro",
    "bzl",
    "cfm",
    "cmake",
    "csproj",
    "cts",
    "editorconfig",
    "ejs",
    "erb",
    "gemspec",
    "gql",
    "graphql",
    "haml",
    "hbs",
    "hcl",
    "j2",
    "jinja",
    "jsonc",
    "jsonl",
    "less",
    "mdx",
    "mk",
    "mts",
    "ndjson",
    "nix",
    "njk",
    "parquet",
    "plist",
    "properties",
    "rake",
    "rego",
    "sbt",
    "sln",
    "sol",
    "styl",
    "tmpl",
    "tpl",
    "tsv",
    "vbs",
    "vcxproj",
    "xaml",
    "xsd",
    "xsl",
    "zig",
];

/// Branch names that identify nobody; kept in merge subjects (#111).
const PUBLIC_BRANCHES: &[&str] = &[
    "main",
    "master",
    "develop",
    "development",
    "dev",
    "trunk",
    "staging",
    "production",
    "release",
    "head",
];

/// Common words that are also given or family names. A token of a
/// multi-word person name on this list is not matched on its own; the full
/// name still is (#111).
const NAME_STOP_WORDS: &[&str] = &[
    "account", "actions", "admin", "and", "april", "archer", "art", "august", "baker", "bell",
    "best", "bill", "bin", "bishop", "black", "bot", "brown", "build", "butler", "carter", "case",
    "chase", "cliff", "cook", "cooper", "dawn", "day", "dean", "del", "den", "der", "dev", "drew",
    "early", "faith", "field", "fine", "fisher", "for", "frank", "gene", "github", "gitlab",
    "glass", "grace", "grant", "gray", "green", "grey", "guy", "hall", "hand", "hill", "hope",
    "hunter", "jack", "joy", "june", "key", "king", "knight", "lane", "law", "little", "long",
    "los", "love", "major", "mark", "mason", "max", "may", "miles", "miller", "name", "nice",
    "noble", "page", "park", "parker", "pat", "penny", "porter", "power", "price", "rain", "ray",
    "rich", "rob", "rod", "root", "rose", "sandy", "service", "sharp", "short", "sky", "small",
    "snow", "spring", "steel", "stone", "storm", "summer", "swift", "system", "team", "temple",
    "test", "the", "turner", "user", "van", "von", "walker", "ward", "white", "will", "winter",
    "wolf", "wood", "young", "your",
];

/// Punctuation that ends a sentence, not a URL or path.
pub(super) fn split_trailing_punct(s: &str) -> (&str, &str) {
    let core = s.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '"']);
    s.split_at(core.len())
}

/// The only word pairs that, joined by one slash, are prose rather than a
/// path (either order, any case) (#111, gate B).
const PROSE_PAIRS: &[(&str, &str)] = &[
    ("and", "or"),
    ("read", "write"),
    ("client", "server"),
    ("input", "output"),
    ("true", "false"),
    ("yes", "no"),
    ("on", "off"),
    ("pass", "fail"),
];

/// Whether a slash-joined token names a directory, file or branch (#111).
///
/// Why: gate B found two-segment directories (`billing/invoices`) and
/// branch names (`feature/foo-bar`, `release/2026-09`) sent in the clear,
/// because only rooted, three-segment or extension-bearing tokens counted.
/// What: every slash token is a path unless it is prose: all segments
/// digits (`1/2`, `2026/09/25`), or exactly two segments where one is a
/// single non-uppercase character (`w/o`, `n/a`) or the two form one of the
/// [`PROSE_PAIRS`] (`and/or`, `read/write`). All-caps pairs (`CI/CD`,
/// `I/O`) are paths too (gate B re-run).
/// Test: `jev_gateb_tests::slash_paths_branches_and_files`,
/// `jev_gateb2_tests::only_fixed_prose_pairs_escape_the_slash_rule`.
pub(super) fn is_path(s: &str) -> bool {
    let segs: Vec<&str> = s.split(['/', '\\']).filter(|x| !x.is_empty()).collect();
    if segs.is_empty() || segs.iter().all(|x| x.bytes().all(|b| b.is_ascii_digit())) {
        return false;
    }
    if let [a, b] = segs.as_slice() {
        // A one-letter lowercase side (`w/o`, `n/a`); `I/O` is a path.
        let one = |x: &str| x.chars().count() == 1 && !x.chars().any(char::is_uppercase);
        let single = one(a) || one(b);
        let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
        let pair = PROSE_PAIRS
            .iter()
            .any(|(x, y)| (a == *x && b == *y) || (a == *y && b == *x));
        if single || pair {
            return false;
        }
    }
    true
}

/// Whether `s` starts with a pseudonym this module emits (`PATH_3`,
/// `HOST_1.md`), so a later pass leaves it alone.
pub(super) fn is_placeholder(s: &str) -> bool {
    const PREFIXES: [&str; 11] = [
        "EMAIL_", "PERSON_", "TICKET_", "URL_", "HOST_", "PATH_", "REPO_", "TERM_", "BRANCH_",
        "CAT_", "ID_",
    ];
    PREFIXES.iter().any(|p| {
        s.strip_prefix(p).is_some_and(|rest| {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            digits > 0 && matches!(rest.as_bytes().get(digits), None | Some(b'.'))
        })
    })
}

/// The extension a pseudonymized file keeps: the last `.`-label of `name`
/// when it is a known file extension (`PATH_1.md`).
pub(super) fn known_ext(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    let known = FILE_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext));
    (!stem.is_empty() && known).then_some(ext)
}

/// Whether a merge-subject branch is a public name (`main`, `origin/main`).
pub(super) fn is_public_branch(name: &str) -> bool {
    let bare = name
        .strip_prefix("origin/")
        .or_else(|| name.strip_prefix("upstream/"))
        .unwrap_or(name);
    PUBLIC_BRANCHES.iter().any(|b| b.eq_ignore_ascii_case(bare))
}

/// What a dotted name is.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Dotted<'a> {
    /// Leave it (versions, `e.g.`, framework names, public file names).
    Keep,
    /// A host name, domain or IPv4 address (`HOST_n`).
    Host,
    /// A file name; the extension is kept (`PATH_n.<ext>`).
    File(&'a str),
    /// A file name without a known extension (`Dockerfile.prod`, `PATH_n`).
    FileName,
}

/// Build-file stems: `<stem>.<anything>` is a file name (#111, gate B).
pub(super) const FILE_STEMS: &[&str] = &[
    "brewfile",
    "caddyfile",
    "containerfile",
    "dockerfile",
    "gemfile",
    "jenkinsfile",
    "justfile",
    "makefile",
    "procfile",
    "rakefile",
    "vagrantfile",
];

/// Last labels that make a dotted name a host in any case (`acme.com`,
/// `Acme.com`, `DB1.CORP.ACME.COM`): TLDs and internal suffixes that are
/// not also ordinary code or English words (#111).
const HOST_SUFFIXES: &[&str] = &[
    "ad",
    "ai",
    "au",
    "aws",
    "biz",
    "br",
    "ca",
    "ch",
    "cluster",
    "cn",
    "co",
    "com",
    "corp",
    "cz",
    "de",
    "dk",
    "edu",
    "es",
    "eu",
    "example",
    "fi",
    "fm",
    "fr",
    "gg",
    "gov",
    "hk",
    "ie",
    "il",
    "infra",
    "internal",
    "intra",
    "intranet",
    "invalid",
    "io",
    "jp",
    "kr",
    "lan",
    "localdomain",
    "localhost",
    "ly",
    "mil",
    "mx",
    "net",
    "nl",
    "nz",
    "org",
    "priv",
    "pt",
    "ru",
    "se",
    "sg",
    "svc",
    "tv",
    "tw",
    "uk",
    "vpn",
    "xyz",
    "za",
];

/// Suffixes that are also internal-network names or ordinary words
/// (`fileserver.local`, `acme.dev`, `window.app`): a name ending in one is
/// a host unless another label is on [`CODE_LABELS`] (#111, gate B re-run:
/// the former two-label exemption for `.dev`, `.app` and the rest leaked).
const WORD_SUFFIXES: &[&str] = &[
    "app", "at", "be", "cloud", "dev", "global", "home", "in", "info", "int", "it", "local", "me",
    "no", "office", "online", "private", "prod", "qa", "site", "so", "stage", "staging", "tech",
    "test", "to", "uat", "us",
];

/// Labels that mark a dotted name ending in a [`WORD_SUFFIXES`] label as
/// code or a member access, never a host (`env.local`, `this.app`,
/// `config.prod`).
const CODE_LABELS: &[&str] = &[
    "app", "args", "config", "console", "context", "ctx", "data", "dev", "document", "e2e", "env",
    "exports", "item", "local", "module", "obj", "options", "opts", "package", "params", "process",
    "prod", "props", "req", "request", "res", "response", "result", "self", "settings", "spec",
    "state", "test", "this", "unit", "user", "value", "window",
];

/// Classify a dotted name (#111).
///
/// Why: a short TLD list let internal names such as `db7.acme.lan2x`
/// through, a rule that made every dotted word a host turned code
/// (`Class.method`, `foo.bar()`) into `HOST_n` (gate B), the suffixes
/// `dev`, `app` and `local` turned `config.local` and `window.app` into
/// hosts, and a lowercase-only rule let `DB1.CORP.ACME.COM` through.
/// What: four numeric labels of at most 255 → `Host`. Otherwise the last
/// label decides. Not purely ASCII-alphabetic → `Keep` (`v1.2.3`). A known
/// file extension → `File` (`README.md`, `PriceTable.tsx`), unless the whole
/// name is a framework (`node.js`) → `Keep`. A build-file stem
/// (`Dockerfile.prod`) → `FileName`. Followed by `(`, or a label in
/// camelCase (`fooBar.baz`) → `Keep`. Suffix lists are compared in any
/// case. A [`HOST_SUFFIXES`] last label → `Host`. A [`WORD_SUFFIXES`] label
/// → `Host` unless another label is on [`CODE_LABELS`] (`acme.dev`,
/// `ACME.LOCAL`; `config.dev`, `window.app` stay). Any other last label →
/// `Host` only for an all-lowercase name whose other label carries a digit
/// or `-` (`acme-fin.corpnet`).
/// Test: `jev_obfuscate_tests::dotted_classifier_table`,
/// `jev_redaction_tests::hosts_but_not_dev_app_local_words`,
/// `jev_review_tests::hosts_in_any_case`,
/// `jev_review_tests::internal_two_label_hosts`,
/// `jev_gateb2_tests::exempt_suffix_domains_are_hosts`.
pub(super) fn classify_dotted(s: &str, next: Option<char>) -> Dotted<'_> {
    if is_placeholder(s) {
        return Dotted::Keep;
    }
    let labels: Vec<&str> = s.split('.').collect();
    let octet = |l: &&str| l.len() <= 3 && l.parse::<u16>().is_ok_and(|n| n <= 255);
    if labels.len() == 4 && labels.iter().all(octet) {
        return Dotted::Host;
    }
    let last = labels.last().copied().unwrap_or("");
    if last.is_empty() || !last.chars().all(|c| c.is_ascii_alphabetic()) {
        return Dotted::Keep;
    }
    if labels.len() >= 2 && FILE_STEMS.contains(&labels[0].to_ascii_lowercase().as_str()) {
        return Dotted::FileName;
    }
    if FILE_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(last)) {
        let framework = NOT_FILES.iter().any(|f| f.eq_ignore_ascii_case(s));
        if framework {
            return Dotted::Keep;
        }
        return Dotted::File(last);
    }
    let camel = |l: &&str| {
        l.as_bytes()
            .windows(2)
            .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase())
    };
    if next == Some('(') || labels.iter().any(camel) {
        return Dotted::Keep;
    }
    let lower = last.to_ascii_lowercase();
    if HOST_SUFFIXES.contains(&lower.as_str()) {
        return Dotted::Host;
    }
    if last.len() < 2 {
        return Dotted::Keep;
    }
    let head = &labels[..labels.len() - 1];
    let marked = head
        .iter()
        .any(|l| l.contains('-') || l.bytes().any(|b| b.is_ascii_digit()));
    let code = head
        .iter()
        .any(|l| CODE_LABELS.contains(&l.to_ascii_lowercase().as_str()));
    // #111 (review round 3, gate B re-run): word suffixes in any case
    // (`ACME.LOCAL`, `acme.dev`) unless a label is code.
    let host = if WORD_SUFFIXES.contains(&lower.as_str()) {
        !code
    } else {
        let lowercase = labels
            .iter()
            .all(|l| !l.bytes().any(|b| b.is_ascii_uppercase()));
        lowercase && marked
    };
    if host {
        Dotted::Host
    } else {
        Dotted::Keep
    }
}

/// Whether an IPv6 candidate followed by `next` is an address (#111).
///
/// What: it parses, carries a digit, and does not run on into a word or
/// another colon group. So `12:30:45` (no parse), a bare `::` (no digit),
/// `f64::MAX` and `f32::EPSILON` (followed by a letter) and a 16-group
/// fingerprint (followed by `:`) are kept.
/// Test: `jev_redaction_tests::ipv6_but_not_lookalikes`.
pub(super) fn is_ipv6(s: &str, next: Option<char>) -> bool {
    let runs_on = next.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':');
    !runs_on && s.parse::<Ipv6Addr>().is_ok() && s.bytes().any(|b| b.is_ascii_digit())
}

/// Whether a person-name token is a stop-list word.
pub(super) fn is_name_stop_word(token: &str) -> bool {
    let lower = token.to_lowercase();
    NAME_STOP_WORDS.contains(&lower.as_str())
}
