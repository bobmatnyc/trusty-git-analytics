//! `tga eval subsample`: a seeded, proportional subset of an existing sample.
//!
//! Why: a full sample is more rows than one person labels in a sitting, and
//! agreement between a human and a second rater is measured on a smaller
//! subset. That subset has to keep the sample's stratum mix and be redrawable.
//! What: [`run_subsample`] reads `sample.jsonl` and its `strata.json`, gives
//! each stratum a share of `size` proportional to its share of the source rows
//! ([`largest_remainder`]), takes that many rows per stratum after a seeded
//! shuffle, and writes the subset's `sample.jsonl`, a blind `labels.csv` and a
//! `strata.json`. No label file is read, so the subset cannot depend on one.
//! Test: `tests::largest_remainder_hits_the_exact_total`,
//! `tests/eval_harness.rs::subsample_is_proportional_and_seeded`.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::draw::seeded_shuffle;
use super::merges::{resolve_merges, scale_out_merges};
use super::records::{SampleRecord, StrataSummary, Stratum, SubsampleOrigin};
use super::sample::{write_json, write_jsonl, write_labels};
use super::score::{read_sample, read_strata};
use super::{io_err, EvalError, Result};

/// Inputs to [`run_subsample`].
#[derive(Debug, Clone)]
pub struct SubsampleParams {
    /// Source `sample.jsonl`.
    pub from: PathBuf,
    /// Source `strata.json`; defaults to the file next to `from`.
    pub strata: Option<PathBuf>,
    /// Rows in the subset.
    pub size: usize,
    /// RNG seed.
    pub seed: u64,
    /// #111: tga database copy used to resolve the merge flag of source rows
    /// that lack one; opened read-only.
    pub db: Option<PathBuf>,
    /// Output directory; must not already hold the three output files.
    pub out: PathBuf,
}

/// What [`run_subsample`] wrote.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SubsampleSummary {
    /// Contents of the subset's `strata.json`.
    pub strata: StrataSummary,
    /// Files written.
    pub files: Vec<PathBuf>,
}

/// Split `size` across groups in proportion to `counts` (Hamilton's method).
///
/// Why: a subset's per-stratum counts must mirror the source's shares and
/// still add up to exactly `size`; rounding each quota can miss the total.
/// What: each group gets `floor(size · count / Σcounts)`; the rows left over
/// go one each to the largest fractional remainders, ties to the earlier
/// group. The result sums to `min(size, Σcounts)` and never exceeds a
/// group's count.
/// Test: `tests::largest_remainder_hits_the_exact_total`.
pub fn largest_remainder(size: usize, counts: &[usize]) -> Vec<usize> {
    let total: u128 = counts.iter().map(|&c| c as u128).sum();
    if total == 0 {
        return vec![0; counts.len()];
    }
    let size = size.min(total as usize);
    let quota = |c: usize| {
        (
            size as u128 * c as u128 / total,
            size as u128 * c as u128 % total,
        )
    };
    let mut alloc: Vec<usize> = counts.iter().map(|&c| quota(c).0 as usize).collect();
    let mut order: Vec<usize> = (0..counts.len()).collect();
    order.sort_by(|&a, &b| quota(counts[b]).1.cmp(&quota(counts[a]).1).then(a.cmp(&b)));
    let left = size - alloc.iter().sum::<usize>();
    for &i in order.iter().take(left) {
        alloc[i] += 1;
    }
    debug_assert_eq!(alloc.iter().sum::<usize>(), size);
    debug_assert!(alloc.iter().zip(counts).all(|(a, c)| a <= c));
    alloc
}

/// Seed of one stratum's shuffle, domain-separated from `tga eval sample`'s.
fn subsample_seed(seed: u64, stratum: Stratum) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(b"tga-eval-subsample\0");
    h.update(&seed.to_le_bytes());
    h.update(stratum.as_str().as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&h.finalize().as_bytes()[..8]);
    u64::from_le_bytes(first)
}

/// Draw the subset and write `sample.jsonl`, `labels.csv` and `strata.json`.
///
/// Why: see the module doc. What: drops merge rows first (#111), resolving
/// rows without a merge flag from `db` and refusing any it cannot resolve, and
/// takes each stratum's merge share out of its population
/// ([`scale_out_merges`]); kept rows are written with `is_merge: false`. It
/// then allocates with [`largest_remainder`] over the non-merge source rows
/// per stratum in [`Stratum::ALL`] order; within a stratum,
/// rows are sorted by SHA and shuffled with a seed-derived stream, and the
/// first `k` are kept. Rows keep their source order in the output. Each
/// row's `weight` becomes stratum population ÷ subset rows in the stratum.
/// The sheet has the same columns and redaction as `tga eval sample`'s,
/// ordered by a seed-salted hash. On Unix a created `out` is mode 0700 and
/// every file 0600.
/// Test: `tests/eval_harness.rs::subsample_is_proportional_and_seeded`,
/// `tests/eval_harness.rs::subsample_drops_merges_resolved_from_the_db`.
///
/// # Errors
///
/// I/O and parse failures; [`EvalError::Invalid`] for a row whose merge
/// status is unknown, a zero size, a size above the non-merge source rows, a
/// duplicated SHA, a stratum with rows but no
/// population in `strata.json`, or an output file that already exists.
pub fn run_subsample(params: &SubsampleParams) -> Result<SubsampleSummary> {
    let all = read_sample(&params.from)?;
    // #111: merges never enter a subset; a row of unknown status is an error.
    let merge_flags = resolve_merges(&all, params.db.as_deref())?;
    let strata_path = params.strata.clone().unwrap_or_else(|| {
        params
            .from
            .parent()
            .unwrap_or(Path::new("."))
            .join("strata.json")
    });
    let mut source_strata = read_strata(&strata_path)?;
    let _estimated = scale_out_merges(&mut source_strata, &all, &merge_flags);
    let source: Vec<SampleRecord> = all
        .into_iter()
        .zip(merge_flags)
        .filter(|&(_, m)| !m)
        .map(|(mut r, _)| {
            r.is_merge = Some(false);
            r
        })
        .collect();
    if params.size == 0 || params.size > source.len() {
        return Err(EvalError::Invalid(format!(
            "--size must be between 1 and the {} non-merge rows of {}",
            source.len(),
            params.from.display()
        )));
    }
    let mut seen = HashSet::new();
    if let Some(dup) = source.iter().find(|r| !seen.insert(r.sha.as_str())) {
        return Err(EvalError::Invalid(format!(
            "{} lists {} twice",
            params.from.display(),
            dup.sha
        )));
    }

    let mut by_stratum: BTreeMap<Stratum, Vec<usize>> = BTreeMap::new();
    for (i, r) in source.iter().enumerate() {
        by_stratum.entry(r.stratum).or_default().push(i);
    }
    for stratum in by_stratum.keys() {
        if source_strata.population_of(*stratum) == 0 {
            return Err(EvalError::Invalid(format!(
                "{} has no population for stratum {}; pass the strata.json written with this sample",
                strata_path.display(),
                stratum.as_str()
            )));
        }
    }
    let counts: Vec<usize> = Stratum::ALL
        .iter()
        .map(|s| by_stratum.get(s).map_or(0, Vec::len))
        .collect();
    let alloc = largest_remainder(params.size, &counts);

    let mut keep = vec![false; source.len()];
    let mut taken: BTreeMap<Stratum, usize> = BTreeMap::new();
    for (stratum, k) in Stratum::ALL.into_iter().zip(alloc) {
        let Some(rows) = by_stratum.get_mut(&stratum) else {
            continue;
        };
        rows.sort_by(|a, b| source[*a].sha.cmp(&source[*b].sha));
        seeded_shuffle(rows, subsample_seed(params.seed, stratum));
        for &i in rows.iter().take(k) {
            keep[i] = true;
        }
        taken.insert(stratum, k);
    }
    let records: Vec<_> = source
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(r, _)| {
            let mut r = r.clone();
            let n = taken.get(&r.stratum).copied().unwrap_or(1).max(1) as f64;
            r.weight = source_strata.population_of(r.stratum) as f64 / n;
            r
        })
        .collect();

    let mut strata = source_strata;
    for (key, counts) in strata.strata.iter_mut() {
        counts.sampled = Stratum::ALL
            .iter()
            .find(|s| s.as_str() == key)
            .and_then(|s| taken.get(s))
            .copied()
            .unwrap_or(0) as u64;
    }
    strata.subsample = Some(SubsampleOrigin {
        seed: params.seed,
        size: records.len() as u64,
        source_size: source.len() as u64,
    });

    let files = ["sample.jsonl", "labels.csv", "strata.json"].map(|f| params.out.join(f));
    if let Some(existing) = files.iter().find(|f| f.exists()) {
        return Err(already_exists(existing));
    }
    create_private_dir(&params.out)?;
    for f in &files {
        create_private_file(f)?;
    }
    let salt = format!("tga-eval-subsample:{}", params.seed);
    write_jsonl(&files[0], &records)?;
    write_labels(&files[1], &records, &salt)?;
    write_json(&files[2], &strata)?;
    Ok(SubsampleSummary {
        strata,
        files: files.to_vec(),
    })
}

/// Create `dir` (mode 0700 on Unix) unless it already exists.
pub(crate) fn create_private_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir).map_err(io_err(dir))
}

/// Create an empty `path` (mode 0600 on Unix), refusing an existing file so a
/// rerun never overwrites a sheet a rater is filling in.
pub(crate) fn create_private_file(path: &Path) -> Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    match opts.open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(already_exists(path)),
        Err(e) => Err(io_err(path)(e)),
    }
}

fn already_exists(path: &Path) -> EvalError {
    EvalError::Invalid(format!(
        "{} already exists; pick an empty --out",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the subset's per-stratum counts are the proportional allocation.
    /// What: 212/85/37/66 of 400 → 53/21/9/17 at N=100 (floors give 99; the
    /// 0.5 remainder wins the last row); equal remainders go to the earlier
    /// group, where per-quota rounding would overshoot (3 × 0.667 → 3) or
    /// undershoot the total; empty groups get nothing.
    /// Test: this function.
    #[test]
    fn largest_remainder_hits_the_exact_total() {
        assert_eq!(
            largest_remainder(100, &[212, 85, 37, 66]),
            vec![53, 21, 9, 17]
        );
        assert_eq!(largest_remainder(2, &[1, 1, 1]), vec![1, 1, 0]);
        assert_eq!(largest_remainder(10, &[5, 5, 5, 5]), vec![3, 3, 2, 2]);
        assert_eq!(largest_remainder(7, &[0, 10, 0, 4]), vec![0, 5, 0, 2]);
        assert_eq!(largest_remainder(14, &[0, 10, 0, 4]), vec![0, 10, 0, 4]);
    }
}
