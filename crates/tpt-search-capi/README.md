# tpt-search-capi

C ABI for `tpt-search`: a plain `extern "C"` surface over an opaque engine handle. Documents and results cross the boundary as UTF-8 JSON, using the same shapes as the HTTP API.

> **Status:** Pre-release (0.1.0), not yet published to crates.io. Part of [tpt-search](../../README.md).

## Features

- Functions over an opaque engine handle
- UTF-8 JSON for documents and results, matching the HTTP API shapes
- Hand-written header at `include/tpt_search.h` (cbindgen is avoided because its MPL-2.0 build dependencies fail `deny.toml`)
- Python smoke test under `python/` that exercises the C surface

## License

Dual-licensed under either of

- [MIT license](../../LICENSE-MIT)
- [Apache License, Version 2.0](../../LICENSE-APACHE)

at your option.
