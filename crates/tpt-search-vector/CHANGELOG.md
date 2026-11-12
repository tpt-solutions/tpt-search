# Changelog

All notable changes to this crate are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- SIMD distance kernels (AVX2 on x86_64, NEON on aarch64).
- HNSW approximate k-nearest-neighbour search.
- Product, scalar and binary quantizers.
