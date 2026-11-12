//! Distance and similarity kernels.
//!
//! Each kernel has a scalar reference implementation and, where available, a
//! SIMD version: AVX2 on x86_64 (selected at runtime) and NEON on aarch64
//! (always available on that architecture). The SIMD and scalar versions are
//! tested against each other.
//!
//! Every metric is exposed as a *distance* where smaller means closer, plus a
//! *score* where larger means closer, which is what callers usually want to see.

use std::sync::OnceLock;

pub use tpt_search_core::DistanceMetric;

/// Which kernel implementation was selected for this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernel {
    /// Portable scalar code.
    Scalar,
    /// AVX2 (x86_64, detected at runtime).
    #[cfg(target_arch = "x86_64")]
    Avx2,
    /// NEON (aarch64).
    #[cfg(target_arch = "aarch64")]
    Neon,
}

/// The kernel used by the functions in this module.
pub fn active_kernel() -> Kernel {
    static KERNEL: OnceLock<Kernel> = OnceLock::new();
    *KERNEL.get_or_init(detect)
}

fn detect() -> Kernel {
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        return Kernel::Avx2;
    }
    #[cfg(target_arch = "aarch64")]
    return Kernel::Neon;
    #[cfg(not(target_arch = "aarch64"))]
    Kernel::Scalar
}

/// Dot product of two vectors. Extra components of the longer slice are ignored.
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    match active_kernel() {
        Kernel::Scalar => scalar::dot(a, b),
        #[cfg(target_arch = "x86_64")]
        // SAFETY: `active_kernel` returns Avx2 only after runtime detection succeeded.
        Kernel::Avx2 => unsafe { x86::dot(a, b) },
        #[cfg(target_arch = "aarch64")]
        // SAFETY: NEON is part of the aarch64 baseline.
        Kernel::Neon => unsafe { arm::dot(a, b) },
    }
}

/// Squared Euclidean distance.
pub fn squared_l2(a: &[f32], b: &[f32]) -> f32 {
    match active_kernel() {
        Kernel::Scalar => scalar::squared_l2(a, b),
        #[cfg(target_arch = "x86_64")]
        // SAFETY: see `dot`.
        Kernel::Avx2 => unsafe { x86::squared_l2(a, b) },
        #[cfg(target_arch = "aarch64")]
        // SAFETY: see `dot`.
        Kernel::Neon => unsafe { arm::squared_l2(a, b) },
    }
}

/// Manhattan (L1) distance.
pub fn manhattan(a: &[f32], b: &[f32]) -> f32 {
    match active_kernel() {
        Kernel::Scalar => scalar::manhattan(a, b),
        #[cfg(target_arch = "x86_64")]
        // SAFETY: see `dot`.
        Kernel::Avx2 => unsafe { x86::manhattan(a, b) },
        #[cfg(target_arch = "aarch64")]
        // SAFETY: see `dot`.
        Kernel::Neon => unsafe { arm::manhattan(a, b) },
    }
}

/// Cosine similarity in `[-1, 1]`. Returns `0.0` if either vector has zero length.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let denom = (dot(a, a) * dot(b, b)).sqrt();
    if denom == 0.0 {
        0.0
    } else {
        (dot(a, b) / denom).clamp(-1.0, 1.0)
    }
}

/// Distance between `a` and `b` under `metric`. Smaller is closer.
pub fn distance(metric: DistanceMetric, a: &[f32], b: &[f32]) -> f32 {
    match metric {
        DistanceMetric::Cosine => 1.0 - cosine_similarity(a, b),
        DistanceMetric::Euclidean => squared_l2(a, b),
        DistanceMetric::DotProduct => -dot(a, b),
        DistanceMetric::Manhattan => manhattan(a, b),
    }
}

/// Converts a distance from [`distance`] into a score where larger is closer.
///
/// Cosine gives the similarity, dot product gives the dot product, and the
/// Euclidean and Manhattan metrics give the negated (unsquared, for Euclidean)
/// distance.
pub fn score_from_distance(metric: DistanceMetric, distance: f32) -> f32 {
    match metric {
        DistanceMetric::Cosine => 1.0 - distance,
        DistanceMetric::Euclidean => -distance.max(0.0).sqrt(),
        DistanceMetric::DotProduct => -distance,
        DistanceMetric::Manhattan => -distance,
    }
}

mod scalar {
    pub fn dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    pub fn squared_l2(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| {
                let d = x - y;
                d * d
            })
            .sum()
    }

    pub fn manhattan(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
    }
}

#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
mod x86 {
    use std::arch::x86_64::*;

    /// Reduces 8 lanes to one sum.
    #[target_feature(enable = "avx2")]
    unsafe fn hsum(v: __m256) -> f32 {
        let lo = _mm256_castps256_ps128(v);
        let hi = _mm256_extractf128_ps(v, 1);
        let s = _mm_add_ps(lo, hi);
        let shuf = _mm_movehdup_ps(s);
        let sums = _mm_add_ps(s, shuf);
        let shuf = _mm_movehl_ps(shuf, sums);
        _mm_cvtss_f32(_mm_add_ss(sums, shuf))
    }

    /// Applies `lane` to each 8-wide block and `tail` to the remainder.
    #[target_feature(enable = "avx2")]
    unsafe fn reduce(
        a: &[f32],
        b: &[f32],
        lane: unsafe fn(__m256, __m256) -> __m256,
        tail: fn(f32, f32) -> f32,
    ) -> f32 {
        let n = a.len().min(b.len());
        let mut acc = _mm256_setzero_ps();
        let mut i = 0;
        while i + 8 <= n {
            // SAFETY: i + 8 <= n <= both lengths, so both loads are in bounds.
            let va = _mm256_loadu_ps(a.as_ptr().add(i));
            let vb = _mm256_loadu_ps(b.as_ptr().add(i));
            acc = _mm256_add_ps(acc, lane(va, vb));
            i += 8;
        }
        let mut sum = hsum(acc);
        while i < n {
            sum += tail(a[i], b[i]);
            i += 1;
        }
        sum
    }

    #[target_feature(enable = "avx2")]
    unsafe fn dot_lane(va: __m256, vb: __m256) -> __m256 {
        _mm256_mul_ps(va, vb)
    }

    #[target_feature(enable = "avx2")]
    unsafe fn sq_lane(va: __m256, vb: __m256) -> __m256 {
        let d = _mm256_sub_ps(va, vb);
        _mm256_mul_ps(d, d)
    }

    #[target_feature(enable = "avx2")]
    unsafe fn abs_lane(va: __m256, vb: __m256) -> __m256 {
        let d = _mm256_sub_ps(va, vb);
        let sign = _mm256_set1_ps(-0.0);
        _mm256_andnot_ps(sign, d)
    }

    /// # Safety
    /// The CPU must support AVX2.
    pub unsafe fn dot(a: &[f32], b: &[f32]) -> f32 {
        reduce(a, b, dot_lane, |x, y| x * y)
    }

    /// # Safety
    /// The CPU must support AVX2.
    pub unsafe fn squared_l2(a: &[f32], b: &[f32]) -> f32 {
        reduce(a, b, sq_lane, |x, y| {
            let d = x - y;
            d * d
        })
    }

    /// # Safety
    /// The CPU must support AVX2.
    pub unsafe fn manhattan(a: &[f32], b: &[f32]) -> f32 {
        reduce(a, b, abs_lane, |x, y| (x - y).abs())
    }
}

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod arm {
    use std::arch::aarch64::*;

    /// Applies `lane` to each 4-wide block and `tail` to the remainder.
    ///
    /// # Safety
    /// NEON must be available (always true on aarch64).
    #[target_feature(enable = "neon")]
    unsafe fn reduce(
        a: &[f32],
        b: &[f32],
        lane: unsafe fn(float32x4_t, float32x4_t) -> float32x4_t,
        tail: fn(f32, f32) -> f32,
    ) -> f32 {
        let n = a.len().min(b.len());
        let mut acc = vdupq_n_f32(0.0);
        let mut i = 0;
        while i + 4 <= n {
            // SAFETY: i + 4 <= n <= both lengths, so both loads are in bounds.
            let va = vld1q_f32(a.as_ptr().add(i));
            let vb = vld1q_f32(b.as_ptr().add(i));
            acc = vaddq_f32(acc, lane(va, vb));
            i += 4;
        }
        let mut sum = vaddvq_f32(acc);
        while i < n {
            sum += tail(a[i], b[i]);
            i += 1;
        }
        sum
    }

    #[target_feature(enable = "neon")]
    unsafe fn dot_lane(va: float32x4_t, vb: float32x4_t) -> float32x4_t {
        vmulq_f32(va, vb)
    }

    #[target_feature(enable = "neon")]
    unsafe fn sq_lane(va: float32x4_t, vb: float32x4_t) -> float32x4_t {
        let d = vsubq_f32(va, vb);
        vmulq_f32(d, d)
    }

    #[target_feature(enable = "neon")]
    unsafe fn abs_lane(va: float32x4_t, vb: float32x4_t) -> float32x4_t {
        vabdq_f32(va, vb)
    }

    /// # Safety
    /// NEON must be available (always true on aarch64).
    pub unsafe fn dot(a: &[f32], b: &[f32]) -> f32 {
        reduce(a, b, dot_lane, |x, y| x * y)
    }

    /// # Safety
    /// NEON must be available (always true on aarch64).
    pub unsafe fn squared_l2(a: &[f32], b: &[f32]) -> f32 {
        reduce(a, b, sq_lane, |x, y| {
            let d = x - y;
            d * d
        })
    }

    /// # Safety
    /// NEON must be available (always true on aarch64).
    pub unsafe fn manhattan(a: &[f32], b: &[f32]) -> f32 {
        reduce(a, b, abs_lane, |x, y| (x - y).abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(len: usize, seed: u32) -> Vec<f32> {
        (0..len)
            .map(|i| {
                let x = (i as u32).wrapping_mul(2654435761).wrapping_add(seed);
                (x % 2000) as f32 / 1000.0 - 1.0
            })
            .collect()
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1e-3 * (1.0 + a.abs().max(b.abs()))
    }

    #[test]
    fn simd_matches_scalar_for_all_tail_lengths() {
        for len in 0..70 {
            let a = sample(len, 1);
            let b = sample(len, 7);
            assert!(close(dot(&a, &b), scalar::dot(&a, &b)), "dot len {len}");
            assert!(close(squared_l2(&a, &b), scalar::squared_l2(&a, &b)), "l2 len {len}");
            assert!(close(manhattan(&a, &b), scalar::manhattan(&a, &b)), "l1 len {len}");
        }
    }

    #[test]
    fn known_values() {
        let a = [1.0, 2.0, 3.0];
        let b = [4.0, 5.0, 6.0];
        assert!(close(dot(&a, &b), 32.0));
        assert!(close(squared_l2(&a, &a), 0.0));
        assert!(close(manhattan(&a, &b), 9.0));
        assert!(close(cosine_similarity(&a, &a), 1.0));
        assert!(close(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]), 0.0));
        assert_eq!(cosine_similarity(&[0.0; 4], &a[..1].repeat(4)), 0.0);
    }

    #[test]
    fn distances_order_closer_vectors_first() {
        let q = [1.0, 0.0];
        let near = [0.9, 0.1];
        let far = [-1.0, 0.0];
        for metric in [
            DistanceMetric::Cosine,
            DistanceMetric::Euclidean,
            DistanceMetric::Manhattan,
        ] {
            assert!(distance(metric, &q, &near) < distance(metric, &q, &far), "{metric:?}");
        }
        // Dot product: larger dot is closer.
        assert!(distance(DistanceMetric::DotProduct, &q, &near) < distance(DistanceMetric::DotProduct, &q, &far));
    }

    #[test]
    fn scores_are_larger_for_closer_vectors() {
        let s = score_from_distance(DistanceMetric::Euclidean, 4.0);
        assert!(close(s, -2.0));
        assert!(close(score_from_distance(DistanceMetric::Cosine, 0.25), 0.75));
    }
}
