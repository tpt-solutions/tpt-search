//! Vector compression: product, scalar, and binary quantization.
//!
//! * [`ProductQuantizer`] splits each vector into `m` subvectors and replaces each
//!   with the index of its nearest k-means centroid (one byte when `k <= 256`).
//!   Queries are scored with asymmetric distance tables, so the query stays full
//!   precision.
//! * [`ScalarQuantizer`] maps each dimension linearly to a byte.
//! * [`BinaryQuantizer`] keeps one bit per dimension (its sign) and compares
//!   with Hamming distance.
//!
//! These are standalone building blocks. The HNSW graph stores full-precision
//! vectors, so quantized codes are for compressed storage and for candidate
//! re-ranking, not for graph traversal.

use serde::{Deserialize, Serialize};
use tpt_search_core::IndexError;

use crate::distance::squared_l2;

/// Largest codebook size; codes are stored as single bytes.
pub const MAX_CENTROIDS: usize = 256;

/// Deterministic xorshift generator used for centroid seeding.
fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state | 1;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// One subspace codebook: `k` centroids of `sub_dim` components each, stored flat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subquantizer {
    /// Dimension of each centroid.
    pub sub_dim: usize,
    /// `k * sub_dim` floats, centroid-major.
    pub centroids: Vec<f32>,
}

impl Subquantizer {
    fn k(&self) -> usize {
        self.centroids.len() / self.sub_dim
    }

    fn centroid(&self, c: usize) -> &[f32] {
        &self.centroids[c * self.sub_dim..(c + 1) * self.sub_dim]
    }

    fn nearest(&self, x: &[f32]) -> usize {
        (0..self.k())
            .min_by(|&a, &b| {
                squared_l2(x, self.centroid(a)).total_cmp(&squared_l2(x, self.centroid(b)))
            })
            .unwrap_or(0)
    }
}

/// Product quantizer with `m` subspaces and up to 256 centroids per subspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProductQuantizer {
    dim: usize,
    subquantizers: Vec<Subquantizer>,
}

impl ProductQuantizer {
    /// Trains codebooks with Lloyd's k-means on `data`.
    ///
    /// `dim` must be divisible by `m`. `iters` is the number of Lloyd iterations
    /// per subspace. Equal inputs and seed give equal codebooks.
    pub fn train(
        data: &[Vec<f32>],
        m: usize,
        k: usize,
        iters: usize,
        seed: u64,
    ) -> Result<Self, IndexError> {
        let first = data
            .first()
            .ok_or_else(|| IndexError::InvalidValue("cannot train on an empty set".into()))?;
        let dim = first.len();
        if m == 0 || dim == 0 || dim % m != 0 {
            return Err(IndexError::InvalidValue(format!(
                "dimension {dim} must be a positive multiple of m={m}"
            )));
        }
        if k == 0 || k > MAX_CENTROIDS {
            return Err(IndexError::InvalidValue(format!(
                "k must be in 1..={MAX_CENTROIDS}"
            )));
        }
        if data.iter().any(|v| v.len() != dim) {
            return Err(IndexError::DimensionMismatch {
                expected: dim,
                got: data.iter().map(Vec::len).find(|&l| l != dim).unwrap_or(0),
            });
        }

        let sub_dim = dim / m;
        let mut state = seed;
        let subquantizers = (0..m)
            .map(|s| {
                let points: Vec<&[f32]> = data
                    .iter()
                    .map(|v| &v[s * sub_dim..(s + 1) * sub_dim])
                    .collect();
                Subquantizer {
                    sub_dim,
                    centroids: kmeans(&points, k.min(points.len()), sub_dim, iters, &mut state),
                }
            })
            .collect();
        Ok(ProductQuantizer { dim, subquantizers })
    }

    /// Full vector dimension.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Number of subspaces (bytes per code).
    pub fn m(&self) -> usize {
        self.subquantizers.len()
    }

    /// Encodes a vector to one centroid index per subspace.
    pub fn encode(&self, vector: &[f32]) -> Result<Vec<u8>, IndexError> {
        self.check_dim(vector)?;
        let sub_dim = self.subquantizers[0].sub_dim;
        Ok(self
            .subquantizers
            .iter()
            .enumerate()
            .map(|(s, sq)| sq.nearest(&vector[s * sub_dim..(s + 1) * sub_dim]) as u8)
            .collect())
    }

    /// Reconstructs the approximate vector for a code.
    pub fn decode(&self, codes: &[u8]) -> Result<Vec<f32>, IndexError> {
        if codes.len() != self.m() {
            return Err(IndexError::DimensionMismatch {
                expected: self.m(),
                got: codes.len(),
            });
        }
        let mut out = Vec::with_capacity(self.dim);
        for (sq, &c) in self.subquantizers.iter().zip(codes) {
            out.extend_from_slice(sq.centroid(c as usize));
        }
        Ok(out)
    }

    /// Precomputes squared distances from `query` to every centroid. Pass the result
    /// to [`asymmetric_squared_l2`](Self::asymmetric_squared_l2) for each code.
    pub fn distance_table(&self, query: &[f32]) -> Result<Vec<f32>, IndexError> {
        self.check_dim(query)?;
        let sub_dim = self.subquantizers[0].sub_dim;
        let mut table = Vec::with_capacity(self.m() * MAX_CENTROIDS);
        for (s, sq) in self.subquantizers.iter().enumerate() {
            let q = &query[s * sub_dim..(s + 1) * sub_dim];
            let start = table.len();
            for c in 0..sq.k() {
                table.push(squared_l2(q, sq.centroid(c)));
            }
            // Pad to the fixed stride so lookups can index by code directly.
            table.resize(start + MAX_CENTROIDS, f32::INFINITY);
        }
        Ok(table)
    }

    /// Squared L2 between the query behind `table` and the vector behind `codes`.
    ///
    /// The table must come from [`distance_table`](Self::distance_table) on this
    /// quantizer. Its stride is the codebook size, so codes must index within each
    /// subspace's codebook.
    pub fn asymmetric_squared_l2(&self, table: &[f32], codes: &[u8]) -> f32 {
        codes
            .iter()
            .enumerate()
            .map(|(s, &c)| table[s * MAX_CENTROIDS + c as usize])
            .sum()
    }

    fn check_dim(&self, vector: &[f32]) -> Result<(), IndexError> {
        if vector.len() != self.dim {
            return Err(IndexError::DimensionMismatch {
                expected: self.dim,
                got: vector.len(),
            });
        }
        Ok(())
    }
}

/// Lloyd's algorithm. Returns `k * dim` centroid floats.
fn kmeans(points: &[&[f32]], k: usize, dim: usize, iters: usize, state: &mut u64) -> Vec<f32> {
    // Seed with distinct data points chosen pseudo-randomly.
    let mut centroids: Vec<f32> = Vec::with_capacity(k * dim);
    let mut chosen: Vec<usize> = Vec::with_capacity(k);
    while chosen.len() < k {
        let idx = (xorshift(state) % points.len() as u64) as usize;
        if !chosen.contains(&idx) {
            chosen.push(idx);
        }
    }
    for idx in &chosen {
        centroids.extend_from_slice(points[*idx]);
    }

    let mut assign = vec![0usize; points.len()];
    for _ in 0..iters {
        for (i, p) in points.iter().enumerate() {
            assign[i] = (0..k)
                .min_by(|&a, &b| {
                    squared_l2(p, &centroids[a * dim..(a + 1) * dim])
                        .total_cmp(&squared_l2(p, &centroids[b * dim..(b + 1) * dim]))
                })
                .unwrap_or(0);
        }
        let mut sums = vec![0f64; k * dim];
        let mut counts = vec![0usize; k];
        for (p, &a) in points.iter().zip(&assign) {
            counts[a] += 1;
            for d in 0..dim {
                sums[a * dim + d] += f64::from(p[d]);
            }
        }
        for c in 0..k {
            // Empty clusters keep their previous centroid.
            if counts[c] > 0 {
                for d in 0..dim {
                    centroids[c * dim + d] = (sums[c * dim + d] / counts[c] as f64) as f32;
                }
            }
        }
    }
    centroids
}

/// Per-dimension linear quantizer to one byte per component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScalarQuantizer {
    min: Vec<f32>,
    step: Vec<f32>,
}

impl ScalarQuantizer {
    /// Learns each dimension's range from `data`.
    pub fn train(data: &[Vec<f32>]) -> Result<Self, IndexError> {
        let first = data
            .first()
            .ok_or_else(|| IndexError::InvalidValue("cannot train on an empty set".into()))?;
        let dim = first.len();
        let mut min = vec![f32::INFINITY; dim];
        let mut max = vec![f32::NEG_INFINITY; dim];
        for v in data {
            if v.len() != dim {
                return Err(IndexError::DimensionMismatch {
                    expected: dim,
                    got: v.len(),
                });
            }
            for d in 0..dim {
                min[d] = min[d].min(v[d]);
                max[d] = max[d].max(v[d]);
            }
        }
        let step = min
            .iter()
            .zip(&max)
            .map(|(lo, hi)| {
                let range = hi - lo;
                // A constant dimension still needs a non-zero step to decode.
                if range > 0.0 {
                    range / 255.0
                } else {
                    1.0
                }
            })
            .collect();
        Ok(ScalarQuantizer { min, step })
    }

    /// Encodes a vector; values outside the trained range are clamped.
    pub fn encode(&self, vector: &[f32]) -> Result<Vec<u8>, IndexError> {
        self.check_dim(vector)?;
        Ok(vector
            .iter()
            .enumerate()
            .map(|(d, &x)| (((x - self.min[d]) / self.step[d]).round().clamp(0.0, 255.0)) as u8)
            .collect())
    }

    /// Reconstructs the approximate vector.
    pub fn decode(&self, codes: &[u8]) -> Result<Vec<f32>, IndexError> {
        if codes.len() != self.min.len() {
            return Err(IndexError::DimensionMismatch {
                expected: self.min.len(),
                got: codes.len(),
            });
        }
        Ok(codes
            .iter()
            .enumerate()
            .map(|(d, &c)| self.min[d] + f32::from(c) * self.step[d])
            .collect())
    }

    /// Squared L2 between a full-precision query and an encoded vector.
    pub fn squared_l2_to_codes(&self, query: &[f32], codes: &[u8]) -> Result<f32, IndexError> {
        let decoded = self.decode(codes)?;
        self.check_dim(query)?;
        Ok(squared_l2(query, &decoded))
    }

    fn check_dim(&self, vector: &[f32]) -> Result<(), IndexError> {
        if vector.len() != self.min.len() {
            return Err(IndexError::DimensionMismatch {
                expected: self.min.len(),
                got: vector.len(),
            });
        }
        Ok(())
    }
}

/// One bit per dimension: set if the component is positive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryCode {
    /// Number of bits (the vector dimension).
    pub len: usize,
    /// Packed bits, least significant bit first.
    pub words: Vec<u64>,
}

/// Sign-bit quantizer compared with Hamming distance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BinaryQuantizer;

impl BinaryQuantizer {
    /// Encodes a vector as its sign bits.
    pub fn encode(&self, vector: &[f32]) -> BinaryCode {
        let mut words = vec![0u64; vector.len().div_ceil(64)];
        for (i, &x) in vector.iter().enumerate() {
            if x > 0.0 {
                words[i / 64] |= 1u64 << (i % 64);
            }
        }
        BinaryCode {
            len: vector.len(),
            words,
        }
    }

    /// Number of differing bits. Returns an error if the lengths differ.
    pub fn hamming(&self, a: &BinaryCode, b: &BinaryCode) -> Result<u32, IndexError> {
        if a.len != b.len {
            return Err(IndexError::DimensionMismatch {
                expected: a.len,
                got: b.len,
            });
        }
        Ok(a.words
            .iter()
            .zip(&b.words)
            .map(|(x, y)| (x ^ y).count_ones())
            .sum())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points(n: usize, dim: usize) -> Vec<Vec<f32>> {
        let mut state = 99u64;
        (0..n)
            .map(|_| {
                (0..dim)
                    .map(|_| (xorshift(&mut state) % 2000) as f32 / 1000.0 - 1.0)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn product_quantizer_round_trip_is_reasonable() {
        let data = points(500, 8);
        let pq = ProductQuantizer::train(&data, 4, 64, 10, 7).unwrap();
        assert_eq!(pq.m(), 4);
        let mut total_err = 0.0f32;
        let mut total_norm = 0.0f32;
        for v in &data {
            let codes = pq.encode(v).unwrap();
            assert_eq!(codes.len(), 4);
            let back = pq.decode(&codes).unwrap();
            total_err += squared_l2(v, &back);
            total_norm += squared_l2(v, &[0.0; 8]);
        }
        // Reconstruction should be much better than the zero vector.
        assert!(
            total_err < 0.5 * total_norm,
            "err {total_err} norm {total_norm}"
        );
    }

    #[test]
    fn asymmetric_distance_matches_decoded_distance() {
        let data = points(300, 8);
        let pq = ProductQuantizer::train(&data, 2, 32, 5, 3).unwrap();
        let q = &data[0];
        let table = pq.distance_table(q).unwrap();
        for v in data.iter().take(20) {
            let codes = pq.encode(v).unwrap();
            let via_table = pq.asymmetric_squared_l2(&table, &codes);
            let via_decode = squared_l2(q, &pq.decode(&codes).unwrap());
            assert!((via_table - via_decode).abs() < 1e-3);
        }
    }

    #[test]
    fn product_quantizer_validates_inputs() {
        let data = points(10, 6);
        assert!(
            ProductQuantizer::train(&data, 4, 8, 1, 0).is_err(),
            "6 not divisible by 4"
        );
        assert!(
            ProductQuantizer::train(&data, 2, 300, 1, 0).is_err(),
            "k too large"
        );
        assert!(ProductQuantizer::train(&[], 2, 8, 1, 0).is_err());
    }

    #[test]
    fn scalar_quantizer_error_is_within_one_step() {
        let data = points(200, 5);
        let sq = ScalarQuantizer::train(&data).unwrap();
        for v in data.iter().take(50) {
            let back = sq.decode(&sq.encode(v).unwrap()).unwrap();
            for (a, b) in v.iter().zip(&back) {
                assert!((a - b).abs() <= 2.0 / 255.0 + 1e-6);
            }
        }
    }

    #[test]
    fn binary_hamming_counts_sign_flips() {
        let bq = BinaryQuantizer;
        let a = bq.encode(&[1.0, -1.0, 2.0, -2.0]);
        let b = bq.encode(&[1.0, 1.0, 2.0, -2.0]);
        assert_eq!(bq.hamming(&a, &b).unwrap(), 1);
        let long_a = bq.encode(&vec![1.0; 130]);
        let long_b = bq.encode(&vec![-1.0; 130]);
        assert_eq!(bq.hamming(&long_a, &long_b).unwrap(), 130);
        assert!(bq.hamming(&a, &long_a).is_err());
    }
}
