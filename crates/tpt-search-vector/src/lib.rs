//! Vector search for `tpt-search`: SIMD distance kernels, an HNSW graph for
//! approximate k-nearest-neighbour search, and product, scalar, and binary
//! quantizers for compressed storage.
//!
//! Distance kernels use AVX2 on x86_64 (chosen at runtime) and NEON on aarch64,
//! with a scalar fallback. The crate uses `unsafe` only for those intrinsics; the
//! crate-level allow below is the carve-out described in `todo.md`.
//!
//! # Example
//!
//! ```
//! use tpt_search_vector::{DistanceMetric, HnswConfig, HnswIndex};
//!
//! let mut index = HnswIndex::new(HnswConfig {
//!     metric: DistanceMetric::Cosine,
//!     ..HnswConfig::default()
//! })?;
//! index.insert(1, vec![1.0, 0.0, 0.0])?;
//! index.insert(2, vec![0.0, 1.0, 0.0])?;
//! index.insert(3, vec![0.9, 0.1, 0.0])?;
//!
//! let hits = index.search(&[1.0, 0.0, 0.0], 2, 10)?;
//! assert_eq!(hits[0].key, 1);
//! assert_eq!(hits[1].key, 3);
//! # Ok::<(), tpt_search_core::IndexError>(())
//! ```

#![allow(unsafe_code)]
#![doc(html_root_url = "https://docs.rs/tpt-search-vector")]

pub mod distance;
pub mod hnsw;
pub mod quantize;

pub use distance::{active_kernel, cosine_similarity, distance, score_from_distance, Kernel};
pub use hnsw::{HnswConfig, HnswIndex, HnswNode, ScoredVector};
pub use quantize::{BinaryCode, BinaryQuantizer, ProductQuantizer, ScalarQuantizer, Subquantizer};
pub use tpt_search_core::DistanceMetric;
