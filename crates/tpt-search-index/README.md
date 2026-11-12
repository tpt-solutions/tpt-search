# tpt-search-index

Scalar search layer for `tpt-search`: an inverted index with BM25 scoring and phrase and boolean queries, columnar storage with aggregations, range indexes, and the `Collection` that keeps them consistent.

> **Status:** Pre-release (0.1.0), not yet published to crates.io. Part of [tpt-search](../../README.md).

## Features

- Inverted index with BM25 scoring and phrase/boolean queries (`inverted`)
- Columnar storage with aggregations (`columnar`)
- Range indexes (`range`)
- `Collection` that keeps the indexes consistent (`collection`)
- Vector field support via `tpt-search-vector`

## License

Dual-licensed under either of

- [MIT license](../../LICENSE-MIT)
- [Apache License, Version 2.0](../../LICENSE-APACHE)

at your option.
