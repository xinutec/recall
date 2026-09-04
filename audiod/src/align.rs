//! Tier-1 cross-source alignment: normalised cross-correlation of energy
//! envelopes. Immune to what breaks coherent methods — AGC changes gain
//! slowly and normalised correlation is scale-invariant, noise suppression
//! mangles phase but preserves when speech happens, and Opus at 32 kbps
//! destroys phase but not the envelope. The clock (segment names, capture
//! epoch) only bootstraps the search window; the audio decides.

/// One measured alignment: how far a source's clock sits from the reference,
/// and how confidently the audio says so.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    /// Seconds to ADD to the source's nominal time to land on the reference's
    /// timeline. Negative: the source's timestamps run late.
    pub offset_s: f64,
    /// Normalised correlation at the peak, in [-1, 1]. Below ~0.3 the two
    /// windows do not describe the same room and the offset means nothing.
    pub peak_r: f64,
}

/// Pearson correlation of two equal-length slices; 0.0 when either side is
/// flat (a silent window correlates with nothing, and must not divide by it).
fn pearson(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (
        a.iter().map(|&x| f64::from(x)).sum::<f64>() / n,
        b.iter().map(|&x| f64::from(x)).sum::<f64>() / n,
    );
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for (&x, &y) in a.iter().zip(b) {
        let (dx, dy) = (f64::from(x) - ma, f64::from(y) - mb);
        cov += dx * dy;
        va += dx * dx;
        vb += dy * dy;
    }
    if va <= f64::EPSILON || vb <= f64::EPSILON {
        return 0.0;
    }
    cov / (va * vb).sqrt()
}

/// Slide `source` against `reference` over lags of ±`max_lag` buckets and
/// return the best-correlating lag as an [`Anchor`], with `bucket_s` giving
/// the time each bucket spans. Both slices share one bucket clock (position i
/// = the same nominal wall-time in each). `None` when the overlap at every
/// lag is shorter than `min_overlap` buckets — a verdict from too little
/// shared audio would be noise wearing a number.
pub fn best_lag(
    reference: &[f32],
    source: &[f32],
    max_lag: usize,
    bucket_s: f64,
    min_overlap: usize,
) -> Option<Anchor> {
    let mut best: Option<Anchor> = None;
    let lag_range = -(max_lag as i64)..=(max_lag as i64);
    for lag in lag_range {
        // source shifted by `lag` buckets against the reference
        let (ref_start, src_start) = if lag >= 0 {
            (lag as usize, 0)
        } else {
            (0, (-lag) as usize)
        };
        let len = reference
            .len()
            .saturating_sub(ref_start)
            .min(source.len().saturating_sub(src_start));
        if len < min_overlap {
            continue;
        }
        let r = pearson(
            &reference[ref_start..ref_start + len],
            &source[src_start..src_start + len],
        );
        // The offset that moves the source ONTO the reference is +lag buckets.
        let candidate = Anchor {
            offset_s: lag as f64 * bucket_s,
            peak_r: r,
        };
        if best.is_none_or(|b| r > b.peak_r) {
            best = Some(candidate);
        }
    }
    best
}
