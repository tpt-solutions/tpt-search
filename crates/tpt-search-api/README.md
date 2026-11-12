# tpt-search-api

HTTP and WebSocket API for `tpt-search`, built on `hyper` directly. Web frameworks such as `axum`, `tonic` and `async-graphql` fail `deny.toml`, so the API speaks HTTP/1.1 and WebSocket itself.

> **Status:** Pre-release (0.1.0), not yet published to crates.io. Part of [tpt-search](../../README.md).

## Features

- HTTP/1.1 endpoints (`http` module) and a WebSocket channel (`ws` module)
- Server runtime with shared engine state (`server`, `state`)
- JSON conversion between wire types and core types (`convert`)
- Runnable `serve` and `load` examples

## License

Dual-licensed under either of

- [MIT license](../../LICENSE-MIT)
- [Apache License, Version 2.0](../../LICENSE-APACHE)

at your option.
