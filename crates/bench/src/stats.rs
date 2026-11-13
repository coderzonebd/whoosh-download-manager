//! Summary statistics and the Mann-Whitney U test.
//!
//! Everything here is plain `f64` arithmetic with no dependencies, so the
//! report can be re-derived by anyone from the raw JSON.

use serde::{Deserialize, Serialize};

/// Median, 90th percentile and minimum of a sample.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub n: usize,
    pub median: f64,
    pub p90: f64,
    pub min: f64,
    pub max: f64,
}

impl Summary {
    /// `None` for an empty sample. NaNs are ignored.
    pub fn of(values: &[f64]) -> Option<Self> {
        let mut v: Vec<f64> = values.iter().copied().filter(|x| !x.is_nan()).collect();
        if v.is_empty() {
            return None;
        }
        v.sort_by(f64::total_cmp);
        Some(Self {
            n: v.len(),
            median: quantile_sorted(&v, 0.5),
            p90: quantile_sorted(&v, 0.9),
            min: v[0],
            max: v[v.len() - 1],
        })
    }
}

/// Quantile of sorted data with linear interpolation between closest ranks
/// (Hyndman & Fan type 7, the default of R and NumPy).
pub fn quantile_sorted(sorted: &[f64], q: f64) -> f64 {
    assert!(!sorted.is_empty(), "quantile of empty sample");
    let h = (sorted.len() - 1) as f64 * q.clamp(0.0, 1.0);
    let lo = h.floor() as usize;
    let hi = h.ceil() as usize;
    sorted[lo] + (h - lo as f64) * (sorted[hi] - sorted[lo])
}

/// Median of unsorted data; `None` when empty.
pub fn median(values: &[f64]) -> Option<f64> {
    Summary::of(values).map(|s| s.median)
}

/// Result of a two-sided Mann-Whitney U test.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MannWhitney {
    /// U statistic of the first sample (number of pairs where `a` < `b`,
    /// ties counting one half). Small U means `a` tends to be smaller.
    pub u: f64,
    /// Two-sided p-value.
    pub p: f64,
    /// `true` when `p` comes from the exact null distribution (no ties,
    /// small samples), `false` for the normal approximation.
    pub exact: bool,
}

/// Largest per-sample size for which the exact distribution is used.
const EXACT_MAX: usize = 30;

/// Two-sided Mann-Whitney U test of `a` against `b`.
///
/// Exact p-value when there are no ties and both samples have at most 30
/// values; otherwise the normal approximation with tie and continuity
/// correction. `None` if either sample is empty.
pub fn mann_whitney(a: &[f64], b: &[f64]) -> Option<MannWhitney> {
    let (n1, n2) = (a.len(), b.len());
    if n1 == 0 || n2 == 0 {
        return None;
    }
    // U_a = #{(i, j) : a_i < b_j} + 0.5 #{a_i == b_j}. Computing it from
    // pairs is O(n1 n2), trivial at benchmark sizes, and avoids rank code.
    let mut u = 0.0;
    let mut ties = false;
    for &x in a {
        for &y in b {
            if x < y {
                u += 1.0;
            } else if x == y {
                u += 0.5;
                ties = true;
            }
        }
    }
    // Ties anywhere in the pooled sample affect the variance too.
    let pooled = {
        let mut p: Vec<f64> = a.iter().chain(b).copied().collect();
        p.sort_by(f64::total_cmp);
        p
    };
    let tie_groups = tie_group_sizes(&pooled);
    ties |= tie_groups.iter().any(|&t| t > 1);

    if !ties && n1 <= EXACT_MAX && n2 <= EXACT_MAX {
        let p = exact_two_sided(n1, n2, u);
        return Some(MannWhitney { u, p, exact: true });
    }

    let (n1f, n2f) = (n1 as f64, n2 as f64);
    let n = n1f + n2f;
    let mean = n1f * n2f / 2.0;
    let tie_term: f64 = tie_groups
        .iter()
        .map(|&t| {
            let t = t as f64;
            t * t * t - t
        })
        .sum();
    let var = n1f * n2f / 12.0 * ((n + 1.0) - tie_term / (n * (n - 1.0)));
    if var <= 0.0 {
        // Every value identical: no evidence of a difference.
        return Some(MannWhitney {
            u,
            p: 1.0,
            exact: false,
        });
    }
    let z = ((u - mean).abs() - 0.5).max(0.0) / var.sqrt();
    let p = (2.0 * normal_sf(z)).min(1.0);
    Some(MannWhitney { u, p, exact: false })
}

fn tie_group_sizes(sorted: &[f64]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i + 1;
        while j < sorted.len() && sorted[j] == sorted[i] {
            j += 1;
        }
        out.push(j - i);
        i = j;
    }
    out
}

/// Exact two-sided p-value: `2 * min(P(U <= u), P(U >= u))`, capped at 1.
fn exact_two_sided(n1: usize, n2: usize, u: f64) -> f64 {
    let dist = u_distribution(n1, n2);
    let total: f64 = dist.iter().sum();
    let u_lo = u.floor() as usize;
    let u_hi = u.ceil() as usize;
    let lower: f64 = dist[..=u_lo.min(dist.len() - 1)].iter().sum();
    let upper: f64 = dist[u_hi.min(dist.len() - 1)..].iter().sum();
    (2.0 * lower.min(upper) / total).min(1.0)
}

/// Number of arrangements giving each value of U for sample sizes n1, n2
/// (index = U, 0..=n1*n2), by the recurrence
/// `f(m, n, u) = f(m-1, n, u-n) + f(m, n-1, u)`.
fn u_distribution(n1: usize, n2: usize) -> Vec<f64> {
    // f[m][n] as a vector over u, built up over m for fixed column n.
    let max_u = n1 * n2;
    // prev[n] = distribution for (m-1, n); start with m = 0: only U = 0.
    let mut prev: Vec<Vec<f64>> = (0..=n2).map(|_| vec![1.0]).collect();
    for m in 1..=n1 {
        let mut cur: Vec<Vec<f64>> = Vec::with_capacity(n2 + 1);
        cur.push(vec![1.0]); // n = 0: only U = 0
        for n in 1..=n2 {
            let len = m * n + 1;
            let mut d = vec![0.0; len];
            // f(m-1, n, u-n)
            for (u, &c) in prev[n].iter().enumerate() {
                d[u + n] += c;
            }
            // f(m, n-1, u)
            for (u, &c) in cur[n - 1].iter().enumerate() {
                d[u] += c;
            }
            cur.push(d);
        }
        prev = cur;
    }
    let d = prev.swap_remove(n2);
    debug_assert_eq!(d.len(), max_u + 1);
    d
}

/// Survival function of the standard normal distribution, 1 - Phi(z).
pub fn normal_sf(z: f64) -> f64 {
    0.5 * erfc(z / std::f64::consts::SQRT_2)
}

/// Complementary error function (W. J. Cody's rational approximations via
/// the Numerical Recipes `erfcc` form; relative error < 1.2e-7).
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let r = t
        * (-z * z - 1.265_512_23
            + t * (1.000_023_68
                + t * (0.374_091_96
                    + t * (0.096_784_18
                        + t * (-0.186_288_06
                            + t * (0.278_868_07
                                + t * (-1.135_203_98
                                    + t * (1.488_515_87
                                        + t * (-0.822_152_23 + t * 0.170_872_77)))))))))
            .exp();
    if x >= 0.0 { r } else { 2.0 - r }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn summary_basics() {
        let s = Summary::of(&[5.0, 1.0, 3.0, 2.0, 4.0]).unwrap();
        assert_eq!(s.n, 5);
        assert_eq!(s.median, 3.0);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.max, 5.0);
        // type 7: h = 4 * 0.9 = 3.6 -> 4 + 0.6 * (5 - 4)
        assert!(close(s.p90, 4.6, 1e-12));
        let even = Summary::of(&[1.0, 2.0, 3.0, 4.0]).unwrap();
        assert_eq!(even.median, 2.5);
        assert!(Summary::of(&[]).is_none());
        assert!(Summary::of(&[f64::NAN]).is_none());
        let one = Summary::of(&[7.0]).unwrap();
        assert_eq!((one.median, one.p90, one.min), (7.0, 7.0, 7.0));
    }

    #[test]
    fn u_distribution_matches_known_counts() {
        // n1 = n2 = 2: U in 0..=4 with counts 1,1,2,1,1 (C(4,2) = 6).
        assert_eq!(u_distribution(2, 2), vec![1.0, 1.0, 2.0, 1.0, 1.0]);
        // Total arrangements = C(n1+n2, n1).
        let d = u_distribution(7, 7);
        assert_eq!(d.iter().sum::<f64>(), 3432.0);
        let d = u_distribution(3, 5);
        assert_eq!(d.iter().sum::<f64>(), 56.0);
        // Symmetric.
        let d = u_distribution(4, 6);
        let n = d.len();
        for i in 0..n {
            assert_eq!(d[i], d[n - 1 - i]);
        }
    }

    #[test]
    fn exact_complete_separation_7_vs_7() {
        let a: Vec<f64> = (1..=7).map(f64::from).collect();
        let b: Vec<f64> = (11..=17).map(f64::from).collect();
        let r = mann_whitney(&a, &b).unwrap();
        assert!(r.exact);
        assert_eq!(r.u, 49.0, "every a below every b");
        // Two-sided: 2 / C(14, 7) = 2 / 3432.
        assert!(close(r.p, 2.0 / 3432.0, 1e-12), "p = {}", r.p);
        assert!(r.p < 0.01, "N = 7 per client can reach p < 0.01");
        let r2 = mann_whitney(&b, &a).unwrap();
        assert_eq!(r2.u, 0.0);
        assert!(close(r2.p, r.p, 1e-15));
    }

    #[test]
    fn exact_matches_reference_values() {
        // scipy.stats.mannwhitneyu([1,2,4,7], [3,5,6,8,9], method="exact")
        // -> statistic (U of a, "greater" convention) = 4, p = 0.19047619...
        let a = [1.0, 2.0, 4.0, 7.0];
        let b = [3.0, 5.0, 6.0, 8.0, 9.0];
        let r = mann_whitney(&a, &b).unwrap();
        assert!(r.exact);
        // Our U counts a < b: 20 pairs - 4 = 16.
        assert_eq!(r.u, 16.0);
        assert!(close(r.p, 0.190_476_190_476, 1e-9), "p = {}", r.p);
        // Identical distributions in alternation: p = 1.
        let r = mann_whitney(&[1.0, 3.0, 5.0], &[2.0, 4.0, 6.0]).unwrap();
        assert!(r.p > 0.5);
    }

    #[test]
    fn normal_approximation_with_ties() {
        // Heavy ties force the approximation.
        let a = [1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 3.0];
        let b = [3.0, 4.0, 4.0, 5.0, 5.0, 6.0, 6.0];
        let r = mann_whitney(&a, &b).unwrap();
        assert!(!r.exact);
        assert!(r.p < 0.01, "p = {}", r.p);
        // By hand: U(a < b) = 47.5, mean 24.5, tie groups 2,2,4,2,2,2 ->
        // var = 49/12 * (15 - 90/182), z = 22.5 / sqrt(var) = 2.92354,
        // p = erfc(z / sqrt 2) = 0.0034608 (scipy's asymptotic method with
        // continuity correction gives the same).
        assert_eq!(r.u, 47.5);
        assert!(close(r.p, 0.003_460_8, 1e-6), "p = {}", r.p);
        // All equal: no evidence.
        let r = mann_whitney(&[2.0; 5], &[2.0; 5]).unwrap();
        assert_eq!(r.p, 1.0);
        assert!(mann_whitney(&[], &[1.0]).is_none());
    }

    #[test]
    fn normal_tail() {
        assert!(close(normal_sf(0.0), 0.5, 1e-7));
        assert!(close(normal_sf(1.959_963_985), 0.025, 1e-6));
        assert!(close(normal_sf(-1.0), 0.841_344_746, 1e-6));
        assert!(close(normal_sf(3.0), 0.001_349_898, 1e-7));
    }
}
