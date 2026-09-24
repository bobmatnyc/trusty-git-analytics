//! `tga eval` — the classifier precision harness (#111).
//!
//! Why: measure how often the classification cascade is right, per rule and
//! per method, from a human-labelled stratified sample.
//! What: `sample` draws the sample from a read-only database copy; `subsample`
//! draws a proportional subset of a sample; `score`
//! turns rater labels into a precision report. The library side lives in
//! [`tga::eval`]; this module only parses flags and prints summaries.
//! Test: `tests/eval_harness.rs`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};

use tga::core::config::Config;
use tga::eval::{self, SampleParams, ScoreParams, Stratum, SubsampleParams};

const PRIVACY: &str = "PRIVACY: every file this command writes contains commit text \
(subjects, bodies, paths, PR titles). Store the output directory privately, outside any \
repository, and delete it when the evaluation is done. Nothing is written unless you \
name the directory with --out.";

/// Arguments for `tga eval`.
#[derive(Args, Debug)]
#[command(after_help = PRIVACY)]
pub struct EvalArgs {
    /// Harness step.
    #[command(subcommand)]
    pub step: EvalSubcommand,
}

/// The two harness steps.
#[derive(Subcommand, Debug)]
pub enum EvalSubcommand {
    /// Draw a stratified, capped, seeded sample of classified commits for labelling.
    #[command(after_help = PRIVACY)]
    Sample(SampleArgs),
    /// Draw a seeded subset of an existing sample, proportional per stratum.
    ///
    /// Writes sample.jsonl, a blind labels.csv and strata.json for the subset.
    /// No label file is read. Score a rater of the subset alongside a rater of
    /// the full sample with `score --sample <source sample.jsonl>`.
    #[command(after_help = PRIVACY)]
    Subsample(SubsampleArgs),
    /// Score rater labels against a sample: precision per rule, method and stratum.
    ///
    /// Valid labels are the categories recorded in strata.json, or the taxonomy
    /// and rule categories of the config when --config is passed explicitly,
    /// plus every predicted category in the sample, `unclear`, `mixed` and
    /// `release_merge`. Those three are reported per label but score as no
    /// answer. Merge commits (2+ parents) are excluded with their labels; a
    /// sample written without merge flags needs --db to resolve them.
    #[command(after_help = PRIVACY)]
    Score(ScoreArgs),
}

/// Flags for `tga eval sample`. The rules come from the global `--config`.
#[derive(Args, Debug)]
pub struct SampleArgs {
    /// A COPY of the tga database; opened read-only, refused if writable.
    /// The global --database flag is not used by this command.
    #[arg(long)]
    pub db: PathBuf,
    /// Window length in weeks, ending at the newest commit in the database.
    #[arg(long, default_value_t = 26)]
    pub weeks: u32,
    /// Total sample size, split equally across non-empty strata.
    #[arg(long, default_value_t = 400)]
    pub size: usize,
    /// RNG seed; the same seed on the same database draws the same sample.
    #[arg(long)]
    pub seed: u64,
    /// Maximum commits per repository and per author within each stratum.
    #[arg(long, default_value_t = 5)]
    pub cap: usize,
    /// Salt for author hashes; generated and saved as salt.txt when omitted.
    #[arg(long)]
    pub salt: Option<String>,
    /// Output directory (private, outside any repository). Required.
    #[arg(long)]
    pub out: PathBuf,
}

/// Flags for `tga eval subsample`.
#[derive(Args, Debug)]
pub struct SubsampleArgs {
    /// sample.jsonl written by `tga eval sample`.
    #[arg(long)]
    pub from: PathBuf,
    /// strata.json of that sample; defaults to the one next to it.
    #[arg(long)]
    pub strata: Option<PathBuf>,
    /// Rows in the subset (at most the source's rows).
    #[arg(long)]
    pub size: usize,
    /// RNG seed; the same seed on the same sample draws the same subset.
    #[arg(long)]
    pub seed: u64,
    /// A COPY of the tga database the sample was drawn from; opened read-only.
    /// Resolves the merge flag of rows that lack one so merges are left out
    /// of the subset (#111). Without it such a sample is refused.
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Output directory (private, outside any repository); must not already
    /// hold sample.jsonl, labels.csv or strata.json. Required.
    #[arg(long)]
    pub out: PathBuf,
}

/// Flags for `tga eval score`.
#[derive(Args, Debug)]
pub struct ScoreArgs {
    /// sample.jsonl written by `tga eval sample`.
    #[arg(long)]
    pub sample: PathBuf,
    /// Rater label file (labels.csv with the `label` column filled; a blank
    /// label is unlabelled). The first file is the scored rater: precision
    /// and coverage use its labels over its rows. Repeat once for a second
    /// rater, whose rows may differ; Cohen's kappa is then reported over the
    /// SHAs both labelled.
    #[arg(long, required = true, num_args = 1)]
    pub labels: Vec<PathBuf>,
    /// Adjudicated labels (same format); they replace the first rater's label
    /// on the rows they name, which that rater must have labelled.
    #[arg(long)]
    pub adjudicated: Option<PathBuf>,
    /// strata.json; defaults to the one next to the sample.
    #[arg(long)]
    pub strata: Option<PathBuf>,
    /// A COPY of the tga database the sample was drawn from; opened read-only.
    /// Resolves the merge flag of rows that lack one (samples written before
    /// merges were excluded) so merges can be dropped. Without it such a
    /// sample is refused rather than scored with merges in it.
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Output directory for report.md and report.json. Required.
    #[arg(long)]
    pub out: PathBuf,
}

fn warn_if_in_repo(out: &Path) {
    let probe = out
        .ancestors()
        .find(|p| p.exists())
        .unwrap_or(Path::new("."));
    if let Ok(repo) = git2::Repository::discover(probe) {
        let root = repo.workdir().unwrap_or(repo.path());
        eprintln!(
            "warning: {} is inside the git work tree {}; these files contain commit text — \
             keep them out of version control",
            out.display(),
            root.display()
        );
    }
}

/// Run `tga eval`.
///
/// `config` is the loaded global config; `config_path` says which file it came
/// from, and `config_explicit` whether the user passed `--config`.
pub fn run(
    args: EvalArgs,
    config: Config,
    config_path: &Path,
    config_explicit: bool,
) -> Result<()> {
    match args.step {
        EvalSubcommand::Sample(a) => run_sample(a, config, config_path),
        EvalSubcommand::Subsample(a) => run_subsample(a),
        EvalSubcommand::Score(a) => run_score(a, config, config_explicit),
    }
}

fn run_sample(a: SampleArgs, config: Config, config_path: &Path) -> Result<()> {
    if !config_path.exists() {
        bail!(
            "config file {} not found — `tga eval sample` evaluates a config's rules, pass it with --config",
            config_path.display()
        );
    }
    warn_if_in_repo(&a.out);
    let summary = eval::run_sample(&SampleParams {
        db: a.db,
        config,
        weeks: a.weeks,
        size: a.size,
        seed: a.seed,
        cap: a.cap,
        salt: a.salt,
        out: a.out.clone(),
    })
    .context("tga eval sample")?;

    let s = &summary.strata;
    println!(
        "Window {} → {} ({} weeks): {} commits, seed {}, cap {}",
        s.window_start, s.window_end, s.weeks, s.population, s.seed, s.cap
    );
    // #111: merges (2+ parents) never enter the eval.
    println!(
        "Excluded {} merge commits (2+ parents) from the window.",
        s.merges_excluded
    );
    println!(
        "{:<14} {:>10} {:>8} {:>10}",
        "stratum", "population", "sampled", "weight"
    );
    let mut total = 0;
    for stratum in Stratum::ALL {
        let c = s.strata.get(stratum.as_str()).cloned().unwrap_or_default();
        total += c.sampled;
        let weight = if c.sampled > 0 {
            format!("{:.2}", c.population as f64 / c.sampled as f64)
        } else {
            "—".into()
        };
        println!(
            "{:<14} {:>10} {:>8} {:>10}",
            stratum.as_str(),
            c.population,
            c.sampled,
            weight
        );
    }
    println!("Sampled {total} of {} requested.", s.requested_size);
    if total < s.requested_size && total < s.population {
        println!(
            "Note: the per-repo and per-author --cap limited the sample; with few repositories or authors, raise --cap."
        );
    }
    if summary.drifted > 0 {
        println!(
            "Note: {} stored verdicts differ from the current rules; the sample measures the current rules.",
            summary.drifted
        );
    }
    if summary.bad_timestamps > 0 {
        println!(
            "Note: {} commits skipped for an unparseable timestamp.",
            summary.bad_timestamps
        );
    }
    for f in &summary.files {
        println!("wrote {}", f.display());
    }
    if let Some(salt) = &summary.salt_file {
        println!(
            "Generated salt saved to {}; pass it with --salt to redraw identical author hashes.",
            salt.display()
        );
    }
    println!(
        "Valid labels: {}, {}",
        s.categories.join(", "),
        eval::score::NO_ANSWER_LABELS.join(", ")
    );
    println!("{PRIVACY}");
    Ok(())
}

fn run_subsample(a: SubsampleArgs) -> Result<()> {
    warn_if_in_repo(&a.out);
    let summary = eval::run_subsample(&SubsampleParams {
        from: a.from,
        strata: a.strata,
        size: a.size,
        seed: a.seed,
        db: a.db,
        out: a.out,
    })
    .context("tga eval subsample")?;
    let s = &summary.strata;
    let origin = s.subsample.clone().unwrap_or_default();
    println!(
        "Subset of {} rows from {}, seed {}",
        origin.size, origin.source_size, origin.seed
    );
    println!("{:<14} {:>10} {:>8}", "stratum", "population", "subset");
    for stratum in Stratum::ALL {
        let c = s.strata.get(stratum.as_str()).cloned().unwrap_or_default();
        if c.sampled > 0 {
            println!(
                "{:<14} {:>10} {:>8}",
                stratum.as_str(),
                c.population,
                c.sampled
            );
        }
    }
    for f in &summary.files {
        println!("wrote {}", f.display());
    }
    println!("{PRIVACY}");
    Ok(())
}

fn run_score(a: ScoreArgs, config: Config, config_explicit: bool) -> Result<()> {
    if a.labels.len() > 2 {
        bail!("pass at most two --labels files");
    }
    warn_if_in_repo(&a.out);
    let categories = if config_explicit {
        Some(eval::config_categories(&config).context("loading categories from --config")?)
    } else {
        None
    };
    let report = eval::run_score(&ScoreParams {
        sample: a.sample,
        strata: a.strata,
        labels: a.labels,
        adjudicated: a.adjudicated,
        categories,
        db: a.db,
        out: a.out.clone(),
    })
    .context("tga eval score")?;

    println!(
        "Sample {} · labelled {} · scored {} · unclear {} · mixed {} · release_merge {} · unresolved {}",
        report.sample_size,
        report.labelled,
        report.scored,
        report.unclear,
        report.mixed,
        report.release_merge,
        report.unresolved_disagreements
    );
    // #111: merges are excluded from the eval; say how many.
    println!("{} rows excluded as merges", report.merges_excluded);
    if report.window_merges_estimated > 0 || report.window_merges_exact.is_some() {
        println!(
            "Window merges: {} estimated from the sample's strata, {} exact from --db",
            report.window_merges_estimated,
            report
                .window_merges_exact
                .map_or_else(|| "—".to_string(), |n| n.to_string())
        );
    }
    if let Some(w) = &report.weighted_accuracy {
        println!(
            "Stratum-weighted accuracy {:.1}% [{:.1}%, {:.1}%]",
            w.estimate * 100.0,
            w.ci_low * 100.0,
            w.ci_high * 100.0
        );
    }
    println!("Abstention share {:.1}%", report.abstention.share * 100.0);
    if let Some(k) = report.kappa.as_ref().and_then(|k| k.kappa) {
        println!("Cohen's kappa {k:.3}");
    }
    println!("wrote {}", a.out.join("report.md").display());
    println!("wrote {}", a.out.join("report.json").display());
    Ok(())
}
