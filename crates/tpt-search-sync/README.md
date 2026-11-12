# tpt-search-sync

Edge-to-cloud sync for `tpt-search`: CRDT documents that converge without coordination, a delta-based sync engine with an offline queue that survives restarts, bandwidth-aware planning and compression, and sync monitoring.

> **Status:** Pre-release (0.1.0), not yet published to crates.io. Part of [tpt-search](../../README.md).

## Features

- CRDT documents (`crdt`) that converge without coordination
- Delta-based sync engine with a durable offline queue (`sync`)
- Bandwidth-aware planning and compression (`bandwidth`)
- Sync monitoring (`monitor`)
- Runnable `sync_report` example

## License

Dual-licensed under either of

- [MIT license](../../LICENSE-MIT)
- [Apache License, Version 2.0](../../LICENSE-APACHE)

at your option.
