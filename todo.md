# tpt-search — Build Todo

> Tracks the full build-out of tpt-search per `spec.txt`. License for every
> crate: `MIT OR Apache-2.0`. Author: TPT Solutions.
>
> **Deviations from `spec.txt`** (resolved with the user before this checklist
> was written — see rationale in each, don't "fix" these back toward the spec
> without re-confirming):
> 1. **No tpt-telos / no "formally verified" framing.** `spec.txt` shows
>    `#[requires]/#[ensures]` proc-macro attributes on plain Rust functions
>    "via tpt-telos". The real `tpt-telos-sdk` crate has no such attribute-macro
>    surface (it's a `.telos` DSL pipeline + a standalone `check_contradictions`
>    solver building block) — wiring it in as spec'd isn't possible as written.
>    Decision: drop formal verification entirely. Query safety (injection
>    prevention, resource bounds, panic-freedom) comes from ordinary Rust
>    guarantees — strong typing, exhaustive validation, `proptest` property
>    tests, `cargo-fuzz` fuzzing. No formal-methods dependency anywhere.
> 2. **`unsafe_code = "forbid"` workspace-wide, with per-crate opt-outs.**
>    Matches the `tpt-math` sibling convention, but `spec.txt` requires raw
>    AVX2/NEON SIMD intrinsics (`tpt-search-vector`) and `memmap2` mmap I/O
>    (`tpt-search-storage`), both of which need `unsafe`. Only those two crates
>    carry an explicit `#![allow(unsafe_code)]`.
> 3. **Raft: wrap `openraft` (MIT), not in-house.** `spec.txt` allows either;
>    an in-house Raft is a multi-month project on its own. `tpt-search-distributed`
>    wraps `openraft` and focuses its own scope on sharding/replication/routing.
> 4. **No new `tpt-mit-hash` crate.** `spec.txt` names it as a checksum
>    dependency but it doesn't exist anywhere in the workspace. `tpt-search-storage`
>    depends directly on an existing dual MIT/Apache-2.0 hash crate (e.g.
>    `xxhash-rust` or `crc32fast`) instead.

---

## Phase 0 — Repo Bootstrap

(one-time; no `tpt-rust-map/template/` exists for this repo, so these are
authored directly, following the `tpt-math` / `tpt-telos` sibling convention)

- [ ] Create root `Cargo.toml`: `[workspace]`, `resolver = "2"`, `members = ["crates/*"]`
      (empty `crates/` to start); `[workspace.package]` with `edition = "2021"`,
      `rust-version`, `license = "MIT OR Apache-2.0"`, `authors = ["TPT Solutions"]`,
      `homepage`/`repository` placeholders
- [ ] `[workspace.lints.rust] unsafe_code = "forbid"` + `[workspace.lints.clippy] all = "warn"`
      (see deviation #2 for the two crates that will override this)
- [ ] `rust-toolchain.toml`
- [ ] `rustfmt.toml`
- [ ] `deny.toml` per `spec.txt`'s licensing policy:
      `allow = ["MIT", "0BSD", "Zlib", "BSD-2-Clause", "BSD-3-Clause", "ISC"]`,
      `deny = ["GPL-3.0", "AGPL-3.0", "Apache-2.0"]` (dual `MIT OR Apache-2.0`
      deps still pass — only Apache-2.0-*only* crates are rejected)
- [ ] CI workflow (`fmt --check`, `clippy --all-targets --all-features -- -D warnings`,
      `test`, `cargo deny check licenses`)
- [ ] `LICENSE-MIT` and `LICENSE-APACHE`
- [ ] Rust `.gitignore` (`/target`, etc.)
- [ ] Root `README.md` stub — mission summary, link to `spec.txt`
- [ ] Create empty `crates/` directory
- [ ] `git init` (local only — no GitHub remote/push)
- [ ] Initial commit
- [ ] Sanity check: `cargo build` succeeds on the empty workspace

## Per-Crate Checklist Template

Every crate phase below repeats this shape (no `no_std` step anywhere —
unlike `tpt-math`, tpt-search is `tokio`/async throughout):

1. Scaffold `crates/<name>/` (Cargo.toml inheriting workspace fields, `lib.rs` stub)
2. Wire dependencies (internal `tpt-search-*` + external crates)
3. Implement scope
4. Unit tests + doctests (+ integration tests where noted)
5. Rustdoc (crate-level + public API)
6. `cargo fmt --check` / `cargo clippy --all-targets --all-features -- -D warnings` clean
7. `cargo deny check` clean

---

## Phase 1 — Core Foundation (Months 1-4)

*Deliverables: `tpt-search-core`, `tpt-search-index`, `tpt-search-storage`, a
scalar-only slice of `tpt-search-query`, unit tests and benchmarks.*
*Milestone: can index and search documents with full-text, range, and boolean queries.*

### tpt-search-core

- [ ] Scaffold `crates/tpt-search-core/`
- [ ] Wire deps: `tokio`, `serde` (use under MIT), `chrono` (or `time`) for `DateTime<Utc>`,
      `tpt-math-units` (typed units for temporal data), `tracing` (logging/metrics)
- [ ] Implement `Document`, `FieldValue`, `Schema`, `FieldSchema`, `FieldType`
- [ ] Implement `Analyzer`, `Tokenizer`, `TokenFilter`
- [ ] Implement `SearchError` and the `SchemaError`/`IndexError`/`QueryError`/
      `StorageError`/`DistributedError`/`SyncError` hierarchy (drop the
      `#[requires]/#[ensures]` contract annotations from spec.txt's `QueryError`
      example per deviation #1 — plain `Result`-based error types only)
- [ ] Async runtime integration helpers (tokio)
- [ ] Logging/metrics scaffolding (`tracing` subscriber setup)
- [ ] Unit tests + doctests
- [ ] Rustdoc
- [ ] `cargo fmt` / `clippy` clean
- [ ] `cargo deny check` clean

### tpt-search-index

- [ ] Scaffold `crates/tpt-search-index/`
- [ ] Wire deps: `tpt-search-core`, `roaring` (RoaringBitmap, use under MIT), `tpt-math-linalg`
- [ ] Implement `InvertedIndex`, `PostingList` (BM25/TF-IDF scoring)
- [ ] Implement `index_document` / `search` / `phrase_search` / `boolean_search`
- [ ] Implement `ScoredDocument`, `Highlight`
- [ ] Implement `ColumnarStore`, `Column` (Integer/Float/Boolean/DateTime), `scan` / `aggregate` / `group_by`
- [ ] Implement `Aggregation` (Count/Sum/Avg/Min/Max/Percentile)
- [ ] Implement `RangeIndex` (BTreeMap + low-cardinality bitmap index): `insert` / `range_query` / `equality_query` / `in_query`
- [ ] Full-text tokenization, stemming (per-language), stop-word filtering, phrase matching
- [ ] Unit tests + doctests
- [ ] Benchmarks (criterion): index throughput, query latency
- [ ] Rustdoc
- [ ] `cargo fmt` / `clippy` clean
- [ ] `cargo deny check` clean

### tpt-search-storage

- [ ] Scaffold `crates/tpt-search-storage/`
- [ ] Wire deps: `tpt-search-core`, `memmap2` (use under MIT), a dual-licensed hash
      crate for checksums (deviation #4 — e.g. `xxhash-rust` or `crc32fast`)
- [ ] `#![allow(unsafe_code)]` carve-out (deviation #2 — needed for `memmap2`)
- [ ] Implement `WriteAheadLog` (`open` / `append` / `replay` / `checkpoint`), `WalEntry`
- [ ] Implement `Segment`, `SegmentManager` (`add_segment` / `search` / `merge_segments` / `flush`)
- [ ] Implement `MergePolicy` (SizeTiered / LogStructured)
- [ ] Implement `MappedSegment` (mmap-backed posting-list / stored-field reads)
- [ ] Snapshot and backup support
- [ ] Unit tests + doctests
- [ ] Benchmarks (criterion): WAL append throughput, segment merge cost
- [ ] Rustdoc
- [ ] `cargo fmt` / `clippy` clean
- [ ] `cargo deny check` clean

### tpt-search-query (scalar-only slice)

- [ ] Scaffold `crates/tpt-search-query/`
- [ ] Wire deps: `tpt-search-core`, `tpt-search-index`
- [ ] Implement `Query` DSL — scalar variants only for now: `Term`, `Range`, `Match`, `Phrase`, `Boolean`
- [ ] Implement `parse_query` for the scalar-only subset of the SQL-like DSL
- [ ] Implement `QueryPlanner.plan()` — basic `Scan` / `IndexLookup` / `Filter` / `Sort` / `Limit` nodes (no cost-based `optimize()` yet — that's Phase 6)
- [ ] Implement `QueryExecutor.execute()` for scalar-only plans
- [ ] Implement `QueryResult` wiring (documents, total_hits, took_ms, highlights)
- [ ] Unit tests + doctests
- [ ] Rustdoc
- [ ] `cargo fmt` / `clippy` clean
- [ ] `cargo deny check` clean

### Phase 1 wrap-up

- [ ] Workspace integration test: index & search documents with full-text, range, and boolean queries end-to-end
- [ ] Benchmarks report (criterion) for index/search throughput
- [ ] **Milestone check:** can index and search documents with full-text, range, and boolean queries

---

## Phase 2 — Vector Search (Months 5-8)

*Deliverables: `tpt-search-vector` with HNSW, PQ/SQ quantization, SIMD distance
(AVX2/NEON), integration with scalar search for hybrid queries.*
*Milestone: can perform k-NN search with cosine similarity at million-vector scale.*

### tpt-search-vector

- [ ] Scaffold `crates/tpt-search-vector/`
- [ ] Wire deps: `tpt-search-core`, `tpt-math-linalg`
- [ ] `#![allow(unsafe_code)]` carve-out (deviation #2 — needed for SIMD intrinsics)
- [ ] Implement `HnswIndex`, `HnswLayer`, `HnswNode`, `DistanceMetric`, `HnswConfig`
- [ ] Implement `insert` / `search` / `batch_insert`, `ScoredVector`
- [ ] Implement `ProductQuantizer` (`train` / `encode` / `decode` / `distance`), `Subquantizer`
- [ ] Implement `ScalarQuantizer` (`train` / `encode` / `decode`)
- [ ] Implement `BinaryQuantizer`
- [ ] SIMD-accelerated distance: `cosine_similarity`, plus Euclidean/dot-product/Manhattan,
      with AVX2 (x86_64), NEON (aarch64), and scalar fallback dispatch
- [ ] Unit tests + doctests
- [ ] Property tests for HNSW recall (proptest)
- [ ] Benchmarks (criterion): insert/search throughput and recall at scale.
      Run any FAISS/Annoy comparison as an **external harness/script only**
      — do not add either as a linked Rust dependency (Annoy is Apache-2.0-only)
- [ ] Rustdoc
- [ ] `cargo fmt` / `clippy` clean
- [ ] `cargo deny check` clean

### tpt-search-query (vector integration)

- [ ] Extend `Query` DSL with `Knn { field, vector, k, ef }`
- [ ] Extend `QueryPlanner` with `PlanNode::VectorSearch`
- [ ] Extend `QueryExecutor` to run vector-search plans
- [ ] Add scalar+vector `Hybrid` query variant with `FusionMethod::ReciprocalRankFusion` (graph fusion arrives in Phase 3)
- [ ] Unit tests + doctests for hybrid scalar+vector queries
- [ ] Rustdoc
- [ ] `cargo fmt` / `clippy` clean
- [ ] `cargo deny check` clean

### Phase 2 wrap-up

- [ ] **Milestone check:** k-NN search with cosine similarity at million-vector scale

---

## Phase 3 — Graph Search (Months 9-12)

*Deliverables: `tpt-search-graph` with property graph model, traversal engine,
Cypher-like query language, graph analytics, integration with scalar+vector search.*
*Milestone: can traverse graphs and perform hybrid scalar+vector+graph queries.*

### tpt-search-graph

- [ ] Scaffold `crates/tpt-search-graph/`
- [ ] Wire deps: `tpt-search-core`, `tpt-math-graph`
- [ ] Implement `PropertyGraph`, `Node`, `Edge`, `PropertyValue`
- [ ] Implement `add_node` / `add_edge` / `get_node` / `get_edge` / `neighbors` / `traverse`
- [ ] Implement `Direction`, `TraversalPattern`, `TraversalFilter`
- [ ] Traversal engine: BFS, DFS, shortest path
- [ ] Implement `GraphQuery`, `MatchPattern`, `NodePattern`, `EdgePattern`, `WhereClause`, `ReturnClause`, `OrderBy`
- [ ] Implement `parse_graph_query` (Cypher-like grammar)
- [ ] Implement `GraphAnalytics`: `shortest_path`, `betweenness_centrality`, `page_rank`,
      `community_detection`, `connected_components` (reuse `tpt-math-graph` algorithms where they fit)
- [ ] Unit tests + doctests
- [ ] Rustdoc
- [ ] `cargo fmt` / `clippy` clean
- [ ] `cargo deny check` clean

### tpt-search-query (graph + full hybrid integration)

- [ ] Extend `Query` DSL with `Graph(GraphQuery)` variant
- [ ] Complete `Hybrid { scalar, vector, graph, fusion }` — all three source types
- [ ] Implement all three `FusionMethod`s: `ReciprocalRankFusion`, `LinearCombination`, `Interleaving`
- [ ] Implement cross-index joins (scalar + vector + graph) in `PlanNode::Join`
- [ ] Extend `QueryPlanner`/`QueryExecutor` with `GraphTraversal` and `Fusion` plan nodes
- [ ] Unit tests + doctests for full hybrid queries
- [ ] Rustdoc
- [ ] `cargo fmt` / `clippy` clean
- [ ] `cargo deny check` clean

### Phase 3 wrap-up

- [ ] **Milestone check:** can traverse graphs and perform hybrid scalar+vector+graph queries

---

## Phase 4 — Distributed Layer (Months 13-18)

*Deliverables: `tpt-search-distributed` (openraft-backed consensus per deviation #3),
shard management, leader-follower replication, query routing and scatter-gather,
cluster management API.*
*Milestone: can run a 3-node cluster with automatic failover and consistent reads.*

### tpt-search-distributed

- [ ] Scaffold `crates/tpt-search-distributed/`
- [ ] Wire deps: `tpt-search-core`, `tpt-search-storage`, `openraft` (MIT — deviation #3)
- [ ] Wrap `openraft`: adapt `RaftNode`/`RaftState`/`RaftMessage` surface onto openraft's
      types, `propose` / `tick` wiring, storage + network trait impls backed by `tpt-search-storage`
- [ ] Implement `ShardManager`, `ShardInfo`, `ShardingStrategy` (Hash / Range / Custom)
- [ ] Implement `route_query` / `route_document` / `rebalance` (+ `ShardMigration`)
- [ ] Leader-follower replication on top of openraft's log replication
- [ ] Implement `QueryRouter` (`route` / `scatter_gather`), `LoadBalancer`
      (RoundRobin / LeastConnections / Random / Weighted)
- [ ] Cluster management API surface (add/remove node, status, health)
- [ ] Unit tests + doctests
- [ ] Integration test: 3-node cluster, kill the leader mid-test, verify automatic
      failover and consistent reads afterward
- [ ] Rustdoc
- [ ] `cargo fmt` / `clippy` clean
- [ ] `cargo deny check` clean

### Phase 4 wrap-up

- [ ] **Milestone check:** 3-node cluster with automatic failover and consistent reads

---

## Phase 5 — Edge-to-Cloud Sync (Months 19-22)

*Deliverables: `tpt-search-sync` with CRDT-based conflict resolution, delta
synchronization, offline queue with replay, bandwidth-aware sync, sync monitoring.*
*Milestone: can sync data between edge devices and cloud with offline capability
and automatic conflict resolution.*

### tpt-search-sync

- [ ] Scaffold `crates/tpt-search-sync/`
- [ ] Wire deps: `tpt-search-core`, `tpt-search-storage`
- [ ] Implement `CrdtDocument`, `LwwDocument` (+ `merge`), `OrSetDocument`, `GCounter`, `PnCounter`
- [ ] Implement `SyncEngine` (`sync` / `get_delta` / `apply_delta` / `resolve_conflicts`)
- [ ] Implement `SyncLog`, `SyncEntry`, `SyncOperation`, `SyncReport`
- [ ] Offline queue with replay (persisted via `tpt-search-storage`'s WAL)
- [ ] Implement `BandwidthManager` (`should_sync` / `compress_delta` / `prioritize`), `SyncPriority`
- [ ] Minimal sync-monitoring surface — a CLI report (pushed/pulled/conflicts/errors);
      no dedicated UI framework in this phase
- [ ] Unit tests + doctests
- [ ] Integration test: simulate an offline/online sync cycle with conflicting edits,
      verify automatic conflict resolution
- [ ] Rustdoc
- [ ] `cargo fmt` / `clippy` clean
- [ ] `cargo deny check` clean

### Phase 5 wrap-up

- [ ] **Milestone check:** sync between edge devices and cloud with offline capability
      and automatic conflict resolution

---

## Phase 6 — API & Polish (Months 23-26)

*Deliverables: `tpt-search-api` with REST + gRPC, WebSocket subscriptions,
cost-based query planner, docs/examples, performance tuning and production hardening.*
*Milestone: production-ready search engine with full API surface.*

### tpt-search-api

- [ ] Scaffold `crates/tpt-search-api/`
- [ ] Wire deps: `tpt-search-query`, `tpt-search-distributed`, `axum` (use under MIT),
      `tonic` (use under MIT), `async-graphql` (use under MIT, for the `/graphql` route)
- [ ] Implement REST routes: `search`, `create_index`, `index_document`, `get_document`,
      `delete_document`, `graphql`, `stream_results`
- [ ] Implement `SearchRequest`/response types, highlight config wiring
- [ ] Implement gRPC `SearchService` trait + `SearchGrpcService` (proto via `tonic-build`)
- [ ] Implement WebSocket real-time subscriptions (axum ws upgrade)
- [ ] Implement `SchemaRegistry` and index-lifecycle endpoints
- [ ] Unit tests + integration tests (HTTP + gRPC + WS) for the full API surface
- [ ] Rustdoc
- [ ] `cargo fmt` / `clippy` clean
- [ ] `cargo deny check` clean

### tpt-search-query (cost-based optimizer)

- [ ] Implement `IndexStatistics` collection
- [ ] Implement `QueryPlanner.optimize()` — cost-based plan rewriting using `IndexStatistics`
- [ ] Implement `QueryExecutor.execute_streaming()`
- [ ] Unit tests + doctests for optimizer decisions (plan selection under varying stats)
- [ ] Rustdoc
- [ ] `cargo fmt` / `clippy` clean
- [ ] `cargo deny check` clean

### Polish

- [ ] Comprehensive documentation pass (rustdoc + `examples/` across all crates)
- [ ] Performance tuning pass (profiling, hot-path optimization)
- [ ] Production hardening: resource limits, graceful shutdown, error-handling audit
- [ ] Load/perf testing harness

### Phase 6 wrap-up

- [ ] **Milestone check:** production-ready search engine with full API surface

---

## Phase 7 — Bindings & Ecosystem (Months 27-30)

*Deliverables: C API, Python bindings, Go bindings, vertical integration examples,
community outreach and adoption.*
*Milestone: drop-in replacement for Elasticsearch in Rust, Python, Go, and C++ ecosystems.*

- [ ] Scaffold `crates/tpt-search-capi/` — C API via `cbindgen`
- [ ] Unit/smoke tests for the C API; rustdoc; `cargo fmt` / `clippy` / `deny` clean
- [ ] Scaffold `crates/tpt-search-py/` — Python bindings via `pyo3`
- [ ] Python-side tests (pytest); packaging (maturin) config
- [ ] Go bindings over `tpt-search-capi` (cgo)
- [ ] Go-side tests and example usage
- [ ] Integration examples for TPT verticals: healthcare, police, aviation, robotics
- [ ] `CONTRIBUTING.md`, `SECURITY.md`, community-facing docs pass
- [ ] Publish crates to crates.io (workspace-wide version bump + release checklist)
- [ ] **Milestone check:** drop-in replacement for Elasticsearch in Rust, Python, Go, and C++ ecosystems

---

## Success Metrics Validation

*Cross-cutting benchmarks from `spec.txt`, tracked independently of any single phase.*

- [ ] Performance: benchmark showing 10× faster than Elasticsearch for equivalent workloads
- [ ] Memory efficiency: benchmark showing 5× lower memory footprint than Elasticsearch
- [ ] Scalability: linear scaling test to 100+ nodes with consistent sub-100ms query latency
- [ ] Hybrid search overhead: measure unified scalar+vector+graph query overhead vs.
      separate systems, confirm <10%
- [ ] Edge sync latency: measure sync latency for typical document updates over a
      simulated 4G network, confirm <5 seconds
- [ ] Licensing: standing `cargo deny check` CI gate proving zero GPL or
      Apache-2.0-only dependencies in the entire dependency tree
- [ ] Adoption: track at least 3 production deployments in TPT verticals within
      12 months of release
