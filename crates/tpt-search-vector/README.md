# tpt-search-vector

Vector search for `tpt-search`: SIMD distance kernels, an HNSW graph for approximate k-nearest-neighbour search, and product, scalar and binary quantizers for compressed storage.

> **Status:** Pre-release (0.1.0), not yet published to crates.io. Part of [tpt-search](../../README.md).

## Features

- Distance kernels (`distance`): AVX2 on x86_64, selected at runtime, and NEON on aarch64
- HNSW approximate k-NN search (`hnsw`)
- Product, scalar and binary quantizers (`quantize`)
- Recall tests and a `scale` example

## License

Dual-licensed under either of

- [MIT license](../../LICENSE-MIT)
- [Apache License, Version 2.0](../../LICENSE-APACHE)

at your option.
