# tpt-search-distributed

Distributed layer for `tpt-search`: shard placement and document routing, load balancing and scatter-gather query routing, and a replicated state machine driven by a Raft implementation written in-house.

> **Status:** Pre-release (0.1.0), not yet published to crates.io. Part of [tpt-search](../../README.md).

## Features

- Shard placement and document routing (`sharding`, `router`)
- Sharded collections (`sharded`)
- Raft consensus with cluster, node and RPC modules (`consensus`)
- Error types (`error`)

Raft is implemented in this crate rather than wrapping `openraft`, because openraft pulls in Apache-2.0-only dependencies that `deny.toml` rejects.

## License

Dual-licensed under either of

- [MIT license](../../LICENSE-MIT)
- [Apache License, Version 2.0](../../LICENSE-APACHE)

at your option.
