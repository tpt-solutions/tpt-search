# tpt-search-query

Query layer for `tpt-search`: a SQL-like query language, a rule-based planner, and an executor over named collections. Scalar, full-text, graph, vector and hybrid fusion queries share one surface.

> **Status:** Pre-release (0.1.0), not yet published to crates.io. Part of [tpt-search](../../README.md).

## Features

- SQL-like parser and AST (`parser`, `ast`)
- Rule-based planner (`plan`) and optimizer (`optimizer`)
- Executor over named collections (`executor`) and collection registry (`registry`)
- Hybrid fusion of scalar, vector and `GRAPH(...)` inputs

## License

Dual-licensed under either of

- [MIT license](../../LICENSE-MIT)
- [Apache License, Version 2.0](../../LICENSE-APACHE)

at your option.
