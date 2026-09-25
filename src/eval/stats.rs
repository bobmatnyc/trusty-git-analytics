//! Estimators for the eval harness: Wilson interval and Cohen's kappa.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Two-sided 95% normal quantile.
pub const Z_95: f64 = 1.959_963_984_540_054;

/// Wilson score interval for `successes` out of `n` trials at quantile `z`.
///
/// Why: per-rule samples are small (often under 30), where the normal
/// approximation gives intervals outside [0, 1]; Wilson stays inside and keeps
/// near-nominal coverage.
/// What: returns `(low, high)`; `None` when `n == 0`.
/// Test: `tests::wilson_matches_known_values`.
pub fn wilson_interval(successes: u64, n: u64, z: f64) -> Option<(f64, f64)> {
    if n == 0 {
        return None;
    }
    let n_f = n as f64;
    let p = successes.min(n) as f64 / n_f;
    let z2 = z * z;
    let denom = 1.0 + z2 / n_f;
    let centre = (p + z2 / (2.0 * n_f)) / denom;
    let half = z * (p * (1.0 - p) / n_f + z2 / (4.0 * n_f * n_f)).sqrt() / denom;
    Some(((centre - half).max(0.0), (centre + half).min(1.0)))
}

/// Cohen's kappa for two raters over the same items.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Kappa {
    /// Items both raters labelled.
    pub n: u64,
    /// Observed agreement.
    pub observed: f64,
    /// Agreement expected by chance from the marginals.
    pub expected: f64,
    /// `(observed - expected) / (1 - expected)`; `None` when `expected == 1`.
    pub kappa: Option<f64>,
}

/// Cohen's kappa over paired labels.
///
/// Why: two raters' agreement bounds how precisely any precision number can be
/// read; kappa corrects raw agreement for chance.
/// What: `pairs` are `(rater_a, rater_b)` labels for the same item. Returns
/// `None` for an empty slice.
/// Test: `tests::kappa_matches_worked_example`.
pub fn cohen_kappa(pairs: &[(String, String)]) -> Option<Kappa> {
    if pairs.is_empty() {
        return None;
    }
    let n = pairs.len() as f64;
    let mut a_marg: BTreeMap<&str, f64> = BTreeMap::new();
    let mut b_marg: BTreeMap<&str, f64> = BTreeMap::new();
    let mut agree = 0.0;
    for (a, b) in pairs {
        *a_marg.entry(a.as_str()).or_default() += 1.0;
        *b_marg.entry(b.as_str()).or_default() += 1.0;
        if a == b {
            agree += 1.0;
        }
    }
    let observed = agree / n;
    let expected: f64 = a_marg
        .iter()
        .map(|(k, ca)| ca / n * b_marg.get(k).copied().unwrap_or(0.0) / n)
        .sum();
    let kappa = if (1.0 - expected).abs() < f64::EPSILON {
        None
    } else {
        Some((observed - expected) / (1.0 - expected))
    };
    Some(Kappa {
        n: pairs.len() as u64,
        observed,
        expected,
        kappa,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 5e-4
    }

    /// Why: the interval is the headline uncertainty on every precision row.
    /// What: 8/10 → [0.490, 0.943]; 0/10 → [0, 0.278]; 10/10 → [0.722, 1];
    /// n = 0 → None.
    /// Test: this function.
    #[test]
    fn wilson_matches_known_values() {
        let (lo, hi) = wilson_interval(8, 10, Z_95).unwrap_or((0.0, 0.0));
        assert!(close(lo, 0.4902) && close(hi, 0.9433), "{lo} {hi}");
        let (lo, hi) = wilson_interval(0, 10, Z_95).unwrap_or((1.0, 1.0));
        assert!(close(lo, 0.0) && close(hi, 0.2775), "{lo} {hi}");
        let (lo, hi) = wilson_interval(10, 10, Z_95).unwrap_or((0.0, 0.0));
        assert!(close(lo, 0.7225) && close(hi, 1.0), "{lo} {hi}");
        assert_eq!(wilson_interval(0, 0, Z_95), None);
    }

    /// Why: kappa gates whether the labels are trustworthy at all.
    /// What: the textbook 50-item yes/no example (20 yes-yes, 5 yes-no,
    /// 10 no-yes, 15 no-no) has p_o = 0.7, p_e = 0.5, kappa = 0.4.
    /// Test: this function.
    #[test]
    fn kappa_matches_worked_example() {
        let mut pairs = Vec::new();
        let mut push = |a: &str, b: &str, k: usize| {
            for _ in 0..k {
                pairs.push((a.to_string(), b.to_string()));
            }
        };
        push("yes", "yes", 20);
        push("yes", "no", 5);
        push("no", "yes", 10);
        push("no", "no", 15);
        let k = cohen_kappa(&pairs).unwrap_or(Kappa {
            n: 0,
            observed: 0.0,
            expected: 0.0,
            kappa: None,
        });
        assert_eq!(k.n, 50);
        assert!(close(k.observed, 0.7));
        assert!(close(k.expected, 0.5));
        assert!(close(k.kappa.unwrap_or(f64::NAN), 0.4));
        assert!(cohen_kappa(&[]).is_none());
    }
}
