//! Seeded, capped, stratified sampling.
//!
//! Why: a precision estimate is only reusable if the sample can be redrawn
//! exactly, and only representative if no single repository or author floods
//! a stratum.
//! What: [`draw`] shuffles each stratum with a seed-derived SplitMix64 stream,
//! walks it greedily under the per-repo and per-author cap, and takes the
//! first `k` survivors, where `k` comes from [`allocate`].
//! Test: `tests::same_seed_same_sample`, `tests::caps_hold_per_stratum`,
//! `tests::allocate_redistributes_small_strata`.

use std::collections::{BTreeMap, HashMap};

use super::records::Stratum;

/// One commit eligible for the sample.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Commit SHA; ties are broken by it so input order never matters.
    pub sha: String,
    /// Repository.
    pub repo: String,
    /// Author identity the cap applies to.
    pub author: String,
    /// Stratum of the commit's verdict.
    pub stratum: Stratum,
}

/// Parameters of one draw.
#[derive(Debug, Clone, Copy)]
pub struct DrawParams {
    /// Total sample size requested.
    pub size: usize,
    /// Maximum commits per repo and per author within a stratum.
    pub cap: usize,
    /// RNG seed.
    pub seed: u64,
}

/// Outcome of [`draw`].
#[derive(Debug, Clone, Default)]
pub struct Draw {
    /// Indices into the candidate slice, grouped by stratum in
    /// [`Stratum::ALL`] order, each group in draw order.
    pub selected: Vec<usize>,
    /// Candidates per stratum.
    pub population: BTreeMap<Stratum, usize>,
    /// Sampled commits per stratum.
    pub sampled: BTreeMap<Stratum, usize>,
}

/// SplitMix64: small, fast, and identical on every platform.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform integer in `0..bound` (`bound > 0`), rejection-sampled.
    fn below(&mut self, bound: u64) -> u64 {
        let zone = u64::MAX - (u64::MAX % bound);
        loop {
            let v = self.next();
            if v < zone {
                return v % bound;
            }
        }
    }
}

fn stratum_seed(seed: u64, stratum: Stratum) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(&seed.to_le_bytes());
    h.update(stratum.as_str().as_bytes());
    let bytes = h.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&bytes.as_bytes()[..8]);
    u64::from_le_bytes(first)
}

/// Fisher-Yates shuffle driven by a SplitMix64 stream seeded with `seed`.
///
/// Callers sort `items` first, so the result depends only on the item set and
/// the seed. Shared by [`draw`] and `eval::subsample`.
pub(crate) fn seeded_shuffle<T>(items: &mut [T], seed: u64) {
    let mut rng = SplitMix64(seed);
    for i in (1..items.len()).rev() {
        let j = rng.below(i as u64 + 1) as usize;
        items.swap(i, j);
    }
}

/// Equal allocation of `size` across strata, capped by each one's capacity.
///
/// Why: equal allocation gives every stratum — including small, low-precision
/// ones — enough labels for a usable interval; a stratum smaller than its
/// share must pass the remainder on rather than shrink the sample.
/// What: water-filling. Strata whose capacity is at most the fair share are
/// filled completely and removed; the rest split what remains evenly, with
/// the remainder going to the earliest strata. The result never exceeds
/// capacity and sums to `min(size, Σ capacity)`.
/// Test: `tests::allocate_redistributes_small_strata`.
pub fn allocate(size: usize, capacity: &[usize]) -> Vec<usize> {
    let mut alloc = vec![0usize; capacity.len()];
    let mut active: Vec<usize> = (0..capacity.len()).filter(|&i| capacity[i] > 0).collect();
    let mut remaining = size;
    while remaining > 0 && !active.is_empty() {
        let share = remaining / active.len();
        let saturated: Vec<usize> = active
            .iter()
            .copied()
            .filter(|&i| capacity[i] <= share)
            .collect();
        if saturated.is_empty() {
            let extra = remaining % active.len();
            for (pos, &i) in active.iter().enumerate() {
                alloc[i] = share + usize::from(pos < extra);
            }
            break;
        }
        for &i in &saturated {
            alloc[i] = capacity[i];
            remaining -= capacity[i];
        }
        active.retain(|i| !saturated.contains(i));
    }
    debug_assert!(alloc.iter().zip(capacity).all(|(a, c)| a <= c));
    alloc
}

/// Draw the stratified sample.
///
/// Why/What: see the module doc. Candidates are sorted by SHA inside each
/// stratum before the seeded shuffle, so the sample depends only on the
/// candidate set and the seed.
/// Test: `tests::same_seed_same_sample`, `tests::caps_hold_per_stratum`.
pub fn draw(candidates: &[Candidate], params: &DrawParams) -> Draw {
    let mut by_stratum: BTreeMap<Stratum, Vec<usize>> = BTreeMap::new();
    for (i, c) in candidates.iter().enumerate() {
        by_stratum.entry(c.stratum).or_default().push(i);
    }

    // Per stratum: the capped greedy walk over the shuffled order. Taking a
    // prefix of it equals running the walk with a smaller target.
    let mut eligible: Vec<(Stratum, Vec<usize>)> = Vec::new();
    for stratum in Stratum::ALL {
        let Some(members) = by_stratum.get_mut(&stratum) else {
            continue;
        };
        members.sort_by(|a, b| candidates[*a].sha.cmp(&candidates[*b].sha));
        seeded_shuffle(members, stratum_seed(params.seed, stratum));
        let mut per_repo: HashMap<&str, usize> = HashMap::new();
        let mut per_author: HashMap<&str, usize> = HashMap::new();
        let mut walk = Vec::new();
        for &i in members.iter() {
            let c = &candidates[i];
            let r = per_repo.entry(c.repo.as_str()).or_default();
            let a = per_author.entry(c.author.as_str()).or_default();
            if *r < params.cap && *a < params.cap {
                *r += 1;
                *a += 1;
                walk.push(i);
            }
        }
        eligible.push((stratum, walk));
    }

    let capacity: Vec<usize> = eligible.iter().map(|(_, w)| w.len()).collect();
    let alloc = allocate(params.size, &capacity);
    let mut out = Draw::default();
    for ((stratum, walk), k) in eligible.into_iter().zip(alloc) {
        out.population
            .insert(stratum, by_stratum.get(&stratum).map_or(0, Vec::len));
        out.sampled.insert(stratum, k);
        out.selected.extend(walk.into_iter().take(k));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::TraceTier;

    fn population() -> Vec<Candidate> {
        let strata = [Stratum::Exact, Stratum::RegexHigh, Stratum::CatchAll];
        (0..600)
            .map(|i| Candidate {
                sha: format!("{:040x}", i * 7919),
                repo: format!("repo{}", i % 13),
                author: format!("dev{}", i % 17),
                stratum: strata[i % 3],
            })
            .collect()
    }

    /// Why: allocation drives every stratum's precision interval width.
    /// What: equal split, remainder to the earliest; a small stratum is
    /// filled and its share redistributed; empty strata get nothing.
    /// Test: this function.
    #[test]
    fn allocate_redistributes_small_strata() {
        assert_eq!(allocate(10, &[100, 100, 100]), vec![4, 3, 3]);
        assert_eq!(allocate(12, &[2, 100, 100]), vec![2, 5, 5]);
        assert_eq!(allocate(12, &[0, 100, 1, 3]), vec![0, 8, 1, 3]);
        assert_eq!(allocate(50, &[3, 4]), vec![3, 4]);
    }

    /// Why: a sample that cannot be redrawn cannot be audited.
    /// What: identical input and seed give identical output regardless of
    /// input order; a different seed gives a different sample.
    /// Test: this function.
    #[test]
    fn same_seed_same_sample() {
        let pop = population();
        let p = DrawParams {
            size: 60,
            cap: 5,
            seed: 42,
        };
        let shas = |pop: &[Candidate], d: &Draw| -> Vec<String> {
            d.selected.iter().map(|&i| pop[i].sha.clone()).collect()
        };
        let a = shas(&pop, &draw(&pop, &p));
        let mut reversed = pop.clone();
        reversed.reverse();
        let b = shas(&reversed, &draw(&reversed, &p));
        assert_eq!(a, b);
        let c = shas(&pop, &draw(&pop, &DrawParams { seed: 43, ..p }));
        assert_eq!(c.len(), a.len());
        assert_ne!(a, c);
    }

    /// Why: the cap keeps one repo or author from dominating a stratum.
    /// What: with cap 2 no (stratum, repo) or (stratum, author) pair exceeds
    /// 2, and the capped capacity (13 repos × 2 bounded by 17 authors × 2)
    /// limits each stratum to 26.
    /// Test: this function.
    #[test]
    fn caps_hold_per_stratum() {
        let pop = population();
        let d = draw(
            &pop,
            &DrawParams {
                size: 300,
                cap: 2,
                seed: 7,
            },
        );
        let mut repo: HashMap<(Stratum, &str), usize> = HashMap::new();
        let mut author: HashMap<(Stratum, &str), usize> = HashMap::new();
        for &i in &d.selected {
            let c = &pop[i];
            *repo.entry((c.stratum, c.repo.as_str())).or_default() += 1;
            *author.entry((c.stratum, c.author.as_str())).or_default() += 1;
        }
        assert!(repo.values().all(|&n| n <= 2));
        assert!(author.values().all(|&n| n <= 2));
        assert!(d.sampled.values().all(|&n| n <= 26));
        assert_eq!(d.population.values().sum::<usize>(), 600);
    }

    /// Why: strata must partition the verdicts.
    /// What: spot-checks each branch of `Stratum::classify`.
    /// Test: this function.
    #[test]
    fn strata_partition_tiers() {
        use Stratum as S;
        use TraceTier as T;
        assert_eq!(S::classify(T::Exact, "feature", 0.95), S::Exact);
        assert_eq!(S::classify(T::Regex, "bugfix", 0.9), S::RegexHigh);
        assert_eq!(S::classify(T::Regex, "bugfix", 0.55), S::RegexMid);
        assert_eq!(S::classify(T::Regex, "bugfix", 0.8), S::RegexOther);
        assert_eq!(S::classify(T::CatchAll, "maintenance", 0.3), S::CatchAll);
        assert_eq!(
            S::classify(T::Unclassified, "uncategorized", 0.0),
            S::Unknown
        );
        assert_eq!(S::classify(T::Fuzzy, "merge", 0.6), S::Fuzzy);
        assert_eq!(S::classify(T::WeightedSum, "test", 0.5), S::WeightedSum);
        assert_eq!(S::classify(T::Llm, "feature", 0.8), S::Other);
    }
}
