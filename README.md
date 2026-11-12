# tpt-search

A Rust search engine that unifies full-text, vector (k-NN), and graph search
behind one query surface, with edge-to-cloud sync and a distributed layer.
Built by TPT Solutions as a drop-in alternative to Elasticsearch for TPT
verticals.

The full design is in [spec.txt](spec.txt). The phased build plan and its
deviations from the spec are tracked in [todo.md](todo.md).

## Workspace

Crates live under `crates/` and are added phase by phase (see `todo.md`).

## Licensing

Dual-licensed under either of

- [MIT license](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option. Dependencies are checked with `cargo deny check licenses`.
