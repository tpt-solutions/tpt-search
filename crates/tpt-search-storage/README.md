# tpt-search-storage

Persistence for `tpt-search`: a checksummed, fsynced write-ahead log; segment files with memory-mapped reads; a segment manager with merge policies; and snapshot and restore with checksum manifests.

> **Status:** Pre-release (0.1.0), not yet published to crates.io. Part of [tpt-search](../../README.md).

## Features

- Checksummed, fsynced write-ahead log (`wal`)
- Segment files (`segment`) with memory-mapped reads (`mapped`)
- Snapshot and restore with checksum manifests (`snapshot`)

The crate allows `unsafe` only for memory-mapped I/O through `memmap2`.

## License

Dual-licensed under either of

- [MIT license](../../LICENSE-MIT)
- [Apache License, Version 2.0](../../LICENSE-APACHE)

at your option.
