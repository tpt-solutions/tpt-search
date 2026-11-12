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
> 3. **Raft: in-house (superseded the original "wrap openraft" choice).** openraft pulls in
>    Apache-2.0-only crates that `deny.toml` rejects, and `async-raft` failed in testing
>    (see Phase 4). `tpt-search-distributed` implements Raft itself. Sharding and routing
>    sit on top of it.
> 4. **No new `tpt-mit-hash` crate.** `spec.txt` names it as a checksum
>    dependency but it doesn't exist anywhere in the workspace. `tpt-search-storage`
>    depends directly on an existing dual MIT/Apache-2.0 hash crate (e.g.
>    `xxhash-rust` or `crc32fast`) instead.

---

## Phase 0 — Repo Bootstrap

(one-time; no `tpt-rust-map/template/` exists for this repo, so these are
authored directly, following the `tpt-math` / `tpt-telos` sibling convention)

- [x] Create root `Cargo.toml`: `[workspace]`, `resolver = "2"`, `members = ["crates/*"]`
      (empty `crates/` to start); `[workspace.package]` with `edition = "2021"`,
      `rust-version`, `license = "MIT OR Apache-2.0"`, `authors = ["TPT Solutions"]`,
      `homepage`/`repository` placeholders
- [x] `[workspace.lints.rust] unsafe_code = "forbid"` + `[workspace.lints.clippy] all = "warn"`
      (see deviation #2 for the two crates that will override this)
- [x] `rust-toolchain.toml`
- [x] `rustfmt.toml`
- [x] `deny.toml` per `spec.txt`'s licensing policy:
      `allow = ["MIT", "0BSD", "Zlib", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Unicode-3.0"]`.
      No `deny` key: cargo-deny v2 rejects anything not in `allow` (Apache-2.0-*only*,
      GPL, AGPL), and dual `MIT OR Apache-2.0` deps pass via their MIT side.
- [x] CI workflow (`fmt --check`, `clippy --all-targets --all-features -- -D warnings`,
      `test`, `cargo deny check licenses`)
- [x] `LICENSE-MIT` and `LICENSE-APACHE`
- [x] Rust `.gitignore` (`/target`, etc.)
- [x] Root `README.md` stub — mission summary, link to `spec.txt`
- [x] Create empty `crates/` directory
- [x] `git init` (local only — no GitHub remote/push)
- [ ] Initial commit
- [x] Sanity check: `cargo build` succeeds on the workspace (verified once `tpt-search-core` landed in Phase 1)

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

**Phase 1 deviations and notes** (recorded as built; revisit when the phase they affect starts):
- `tpt-math-units` / `tpt-math-linalg` are not wired in yet. Phase 1 has no
  dimensional or vectorized math to route through them, and wiring them now only
  adds weight. `tpt-math-linalg` belongs with Phase 2's SIMD distance work.
- `tpt-search-storage` depends on `tpt-search-index` (not only `core`) because
  `Segment` holds a `Collection`, as spec.txt's `Segment` does. This is a
  dependency the todo's list omitted.
- Signature changes from the spec sketch: `InvertedIndex::index_document` returns
  the row number; `ScoredDocument` carries an internal `row`; `ColumnarStore::group_by`
  takes an aggregated field; `RangeIndex` takes pre-converted `IndexKey`s;
  `WriteAheadLog::replay` returns `(sequence, entry)` pairs; `SegmentManager::search`
  takes a `TermQuery`/`BooleanQuery` instead of the query-layer `Query` (storage
  must not depend on the query crate); `SegmentManager::merge_segments` returns the
  new `SegmentId`.
- The WAL is a single file, not rotated segments, so `current_segment` is absent.
  `checkpoint` rewrites the log to one marker record, atomically.
- `Phrase` has no `slop` parameter yet. Only exact adjacent phrases are supported.
- `Predicate::Not` is evaluated over live rows. `should` clauses in a query with
  `must` clauses only affect scores in the index-level `boolean_search`, not in the
  planner (documented in `plan.rs`).
- `QueryExecutor` borrows an `IndexRegistry` rather than holding an `Arc`, since
  Phase 1 has no concurrent writers. Revisit with the API layer in Phase 6.
- `execute_streaming` and `QueryPlanner::optimize` are Phase 6 items.
- Deleted rows still count toward BM25 document frequency and average length until a
  merge reclaims them. This slightly lowers scores after heavy churn.
- Scores are computed per segment, so merged cross-segment results can rank
  slightly differently from a single merged index.

### tpt-search-core

- [x] Scaffold `crates/tpt-search-core/`
- [x] Wire deps: `tokio`, `serde` (use under MIT), `chrono` (or `time`) for `DateTime<Utc>`,
      `tpt-math-units` (typed units for temporal data), `tracing` (logging/metrics)
- [x] Implement `Document`, `FieldValue`, `Schema`, `FieldSchema`, `FieldType`
- [x] Implement `Analyzer`, `Tokenizer`, `TokenFilter`
- [x] Implement `SearchError` and the `SchemaError`/`IndexError`/`QueryError`/
      `StorageError`/`DistributedError`/`SyncError` hierarchy (drop the
      `#[requires]/#[ensures]` contract annotations from spec.txt's `QueryError`
      example per deviation #1 — plain `Result`-based error types only)
- [x] Async runtime integration helpers (tokio)
- [x] Logging/metrics scaffolding (`tracing` subscriber setup)
- [x] Unit tests + doctests
- [x] Rustdoc
- [x] `cargo fmt` / `clippy` clean
- [x] `cargo deny check` clean

### tpt-search-index

- [x] Scaffold `crates/tpt-search-index/`
- [x] Wire deps: `tpt-search-core`, `roaring` (RoaringBitmap, use under MIT), `tpt-math-linalg`
- [x] Implement `InvertedIndex`, `PostingList` (BM25/TF-IDF scoring)
- [x] Implement `index_document` / `search` / `phrase_search` / `boolean_search`
- [x] Implement `ScoredDocument`, `Highlight`
- [x] Implement `ColumnarStore`, `Column` (Integer/Float/Boolean/DateTime), `scan` / `aggregate` / `group_by`
- [x] Implement `Aggregation` (Count/Sum/Avg/Min/Max/Percentile)
- [x] Implement `RangeIndex` (BTreeMap + low-cardinality bitmap index): `insert` / `range_query` / `equality_query` / `in_query` — bitmap index deferred (see Phase 1 notes)
- [x] Full-text tokenization, stemming (per-language), stop-word filtering, phrase matching
- [x] Unit tests + doctests
- [x] Benchmarks (criterion): index throughput, query latency
- [x] Rustdoc
- [x] `cargo fmt` / `clippy` clean
- [x] `cargo deny check` clean

### tpt-search-storage

- [x] Scaffold `crates/tpt-search-storage/`
- [x] Wire deps: `tpt-search-core`, `memmap2` (use under MIT), a dual-licensed hash
      crate for checksums (deviation #4 — e.g. `xxhash-rust` or `crc32fast`)
- [x] `#![allow(unsafe_code)]` carve-out (deviation #2 — needed for `memmap2`)
- [x] Implement `WriteAheadLog` (`open` / `append` / `replay` / `checkpoint`), `WalEntry`
- [x] Implement `Segment`, `SegmentManager` (`add_segment` / `search` / `merge_segments` / `flush`)
- [x] Implement `MergePolicy` (SizeTiered / LogStructured)
- [ ] Implement `MappedSegment` (mmap-backed posting-list / stored-field reads) — partial: mmap + checksum + full decode only; per-term offset reads deferred (see Phase 1 notes)
- [x] Snapshot and backup support
- [x] Unit tests + doctests
- [x] Benchmarks (criterion): WAL append throughput, segment merge cost
- [x] Rustdoc
- [x] `cargo fmt` / `clippy` clean
- [x] `cargo deny check` clean

### tpt-search-query (scalar-only slice)

- [x] Scaffold `crates/tpt-search-query/`
- [x] Wire deps: `tpt-search-core`, `tpt-search-index`
- [x] Implement `Query` DSL — scalar variants only for now: `Term`, `Range`, `Match`, `Phrase`, `Boolean`
- [x] Implement `parse_query` for the scalar-only subset of the SQL-like DSL
- [x] Implement `QueryPlanner.plan()` — basic `Scan` / `IndexLookup` / `Filter` / `Sort` / `Limit` nodes (no cost-based `optimize()` yet — that's Phase 6)
- [x] Implement `QueryExecutor.execute()` for scalar-only plans
- [x] Implement `QueryResult` wiring (documents, total_hits, took_ms, highlights)
- [x] Unit tests + doctests
- [x] Rustdoc
- [x] `cargo fmt` / `clippy` clean
- [x] `cargo deny check` clean

### Phase 1 wrap-up

- [x] Workspace integration test: index & search documents with full-text, range, and boolean queries end-to-end
- [x] Benchmarks report (criterion) for index/search throughput — baseline in "Phase 1 benchmark baseline" below

**Phase 1 benchmark baseline** (`cargo bench`, release profile, criterion, short run: 1s warm-up, 3s measurement; Windows, single machine). Numbers are a baseline for tracking regressions, not tuned results. No targets were set in Phase 1.

| Benchmark | Result (median) |
|---|---|
| `index_throughput/upsert_5k_docs` | 477 ms (about 10k docs/s) |
| `query_latency_50k/term` ("rust", about 80% of 50k docs match) | 32 ms |
| `query_latency_50k/phrase` | 27 ms |
| `query_latency_50k/boolean` (must + must_not) | 166 ms |
| `query_latency_50k/range_indexed` | 162 µs |
| `wal/append_fsync` | 300 µs |
| `segment_merge/merge_8_segments_x_1k_docs` | 242 ms |

Term and boolean queries are dominated by materializing tens of thousands of hits
(a `HashMap` entry plus a highlight `Vec` per hit). Tuning that is Phase 6 work.
- [x] **Milestone check:** can index and search documents with full-text, range, and boolean queries

---

## Phase 2 — Vector Search (Months 5-8)

*Deliverables: `tpt-search-vector` with HNSW, PQ/SQ quantization, SIMD distance
(AVX2/NEON), integration with scalar search for hybrid queries.*
*Milestone: can perform k-NN search with cosine similarity at million-vector scale.*

**Phase 2 deviations and notes** (recorded as built):
- `tpt-math-linalg` is not wired in. The distance kernels live in this crate (AVX2 and
  NEON with runtime dispatch), and I did not review `tpt-math-linalg`'s API before
  deciding to keep the crate's dependency set small.
- `HnswLayer` is not a separate type. Each `HnswNode` stores its neighbour list per
  layer, which is the same data without an extra wrapper.
- `DistanceMetric` lives in `tpt-search-core`, so schemas can name it without a
  dependency cycle. The vector crate re-exports it.
- Cosine indexes store unit-length vectors, so a cosine distance is one dot product
  (`1 - dot`). Scores are unchanged.
- Quantizers are standalone (PQ with asymmetric distance tables, scalar, binary).
  HNSW stores full-precision vectors, so quantized codes are not used for graph
  traversal yet.
- The NEON kernels are type-checked for `aarch64-apple-darwin` but have not been run.
  This machine is Windows x86_64.
- Neighbour pruning uses the diversity heuristic. A nearest-only shortcut was
  measured and rejected: it cut clustered-data recall from 1.00 to 0.76 because it
  removed the links that bridge clusters.
- Search uses an accept predicate, so deleted or superseded rows are filtered inside
  the graph search rather than after it.

**Phase 2 benchmark baseline** (`cargo run --release` example and `cargo bench`; Windows,
single machine, single thread; recall is tie-aware against exact cosine search with
200 queries, k=10):

| Workload (128-d unless noted) | Build | Recall@10 at ef 100 / 200 / 400 | Query p50 at ef 100 / 200 / 400 |
|---|---|---|---|
| 1M clustered (1000 centres, realistic embedding-like data) | 412 s (2,427 inserts/s) | 0.981 / 0.982 / 0.986 | 0.38 / 0.30 / 0.35 ms |
| 1M uniform random (adversarial: no low-dimensional structure) | 2,735 s (366 inserts/s)* | 0.093 / 0.165 / 0.285 | 1.57 / 2.27 / 3.65 ms* |
| 100k clustered | 45 s* | 1.000 at all three | 0.16 ms* |
| 16-d uniform, 100k (graph sanity check) | 22 s* | 0.9995 / 1.000 / 1.000 | 0.26 ms* |

* Measured before the kernel speed-up (criterion below), so build time and latency
are pessimistic. Recall does not depend on kernel speed. The 1M clustered row was
re-run on the final code.

Criterion (`cargo bench -p tpt-search-vector`, short 2–3 s windows, median):

| Benchmark | Time |
|---|---|
| `distance_kernels/dot/128` (AVX2) | 8.0 ns |
| `distance_kernels/squared_l2/128` | 9.9 ns |
| `distance_kernels/cosine_distance/128` | 31 ns |
| `distance_kernels/dot/768` | 41 ns |
| `hnsw/build_20k_x128` | 4.3 s |
| `hnsw/search_k10_ef100_20k` | 137 µs |

Open problem: on uniform random 128-d data, recall@10 is low (0.09 at ef=100, 0.29 at
ef=400, 1M vectors). The graph itself is sound, as shown by the 16-d and clustered
results. Uniform random vectors in 128 dimensions have almost no neighbourhood
structure, which is the known hard case for graph indexes. I have not yet separated
"inherent difficulty" from "a tuning issue". Next steps would be a larger `m`, a larger
`ef_construction`, or quantized re-ranking.

### tpt-search-vector

- [x] Scaffold `crates/tpt-search-vector/`
- [ ] Wire deps: `tpt-search-core`, `tpt-math-linalg` — `tpt-math-linalg` not wired (see notes)
- [x] `#![allow(unsafe_code)]` carve-out (deviation #2 — needed for SIMD intrinsics)
- [x] Implement `HnswIndex`, `HnswNode`, `DistanceMetric`, `HnswConfig` (`HnswLayer` folded into `HnswNode`)
- [x] Implement `insert` / `search` / `batch_insert`, `ScoredVector` (plus `search_filtered`)
- [x] Implement `ProductQuantizer` (`train` / `encode` / `decode` / `distance_table`), `Subquantizer`
- [x] Implement `ScalarQuantizer` (`train` / `encode` / `decode`)
- [x] Implement `BinaryQuantizer`
- [x] SIMD-accelerated distance: `cosine_similarity`, plus Euclidean/dot-product/Manhattan,
      with AVX2 (x86_64, runtime-detected), NEON (aarch64, type-checked, not run here), and scalar fallback
- [x] Unit tests + doctests
- [x] Property tests for HNSW recall (proptest)
- [x] Benchmarks (criterion): distance kernels, HNSW build and query; scale example at 100k and 1M
- [ ] FAISS/Annoy comparison harness (external script only) — not done
- [x] Rustdoc
- [x] `cargo fmt` / `clippy` clean
- [x] `cargo deny check` clean

### tpt-search-query (vector integration)

- [x] Extend `Query` DSL with `Knn { field, vector, k, ef }` (`KNN(field, [..], k=N[, ef=M])`)
- [x] Extend `QueryPlanner` with `PlanNode::VectorSearch`
- [x] Extend `QueryExecutor` to run vector-search plans
- [x] Add scalar+vector `Hybrid` query variant with `FusionMethod::ReciprocalRankFusion` (graph fusion arrives in Phase 3)
- [x] Unit tests + doctests for hybrid scalar+vector queries
- [x] Rustdoc
- [x] `cargo fmt` / `clippy` clean
- [x] `cargo deny check` clean

### Phase 2 wrap-up

- [x] **Milestone check:** k-NN search with cosine similarity at million-vector scale — met on clustered 1M vectors (recall@10 0.98 at ef=100). Uniform random 1M vectors remain low-recall (see the open problem above).

---

## Phase 3 — Graph Search (Months 9-12)

*Deliverables: `tpt-search-graph` with property graph model, traversal engine,
Cypher-like query language, graph analytics, integration with scalar+vector search.*
*Milestone: can traverse graphs and perform hybrid scalar+vector+graph queries.*

**Phase 3 deviations and notes** (recorded as built):
- `tpt-math-graph` is not wired in. The graph algorithms are implemented in-crate
  (BFS, DFS, Brandes betweenness, PageRank, label propagation). I did not review its
  API before deciding on the dependency set.
- The graph crate depends on `serde` and `thiserror` only. Its errors are its own
  `GraphError`, which the query layer maps onto `QueryError`.
- Graph node ids are document ids. A graph match is joined to documents by id in the
  `GraphMatch` plan node, which is the cross-index join. There is no general
  `PlanNode::Join`, since no other input needs one yet.
- `traverse` is provided as the free functions `bfs` and `dfs` rather than a method.
  `MatchPattern`, `WhereClause`, and `ReturnClause` are folded into `GraphQuery`
  fields. `GraphAnalytics` is a module of functions.
- `shortest_path` is unweighted (BFS). Weighted paths (Dijkstra) are not built.
- Variable-length hops use layered expansion with per-layer deduplication. This finds
  reachable nodes, not every distinct path, so counts can differ from Cypher's
  path semantics. Edge variables are parsed but not bound in results.
- Community detection is deterministic label propagation. Louvain is not built.
- Graphs live in the registry, next to their collection. They are not persisted by
  the storage layer yet. The graph types derive `serde`, so persistence is a
  follow-up.
- `execute_streaming` is still a Phase 6 item.

**Phase 3 test coverage:** the graph crate has 22 unit tests and 1 doctest (traversal
order, filters, shortest paths, PageRank, betweenness, components, communities, pattern
matching, parse errors, and the match cap). The query crate has 8 graph integration
tests, which run `GRAPH("...")` through parse, plan, and execute. They cover joins to
documents, variable-length hops, `LINEAR` and `INTERLEAVE` fusion with a scalar input,
and error cases.

### tpt-search-graph

- [x] Scaffold `crates/tpt-search-graph/`
- [ ] Wire deps: `tpt-search-core`, `tpt-math-graph` — neither is wired (see notes)
- [x] Implement `PropertyGraph`, `Node`, `Edge`, `PropertyValue`
- [x] Implement `add_node` / `add_edge` / `get_node` / `get_edge` / `neighbors` (`traverse` is `bfs` / `dfs`)
- [x] Implement `Direction`, `TraversalPattern`, `TraversalFilter`
- [x] Traversal engine: BFS, DFS, shortest path
- [x] Implement `GraphQuery`, `NodePattern`, `EdgePattern`, `OrderBy`, `Expr` (`MatchPattern`, `WhereClause`, `ReturnClause` folded into `GraphQuery`)
- [x] Implement `parse_graph_query` (Cypher-style grammar subset)
- [x] Implement `GraphAnalytics`: `shortest_path`, `betweenness_centrality`, `pagerank`, `label_propagation`, `connected_components` (in-crate; `tpt-math-graph` not used)
- [x] Unit tests + doctests
- [x] Rustdoc
- [x] `cargo fmt` / `clippy` clean
- [x] `cargo deny check` clean

### tpt-search-query (graph + full hybrid integration)

- [x] Extend `Query` DSL with `Graph(GraphQuery)` variant (`GRAPH("MATCH ... RETURN ...")`)
- [x] Complete `Hybrid { scalar, vector, graph, fusion }` — all three source types
- [x] Implement all three `FusionMethod`s: `ReciprocalRankFusion`, `LinearCombination` (`LINEAR(w, ...)`), `Interleaving` (`INTERLEAVE`)
- [x] Implement cross-index joins (scalar + vector + graph) — as the `GraphMatch` node's id join; no general `PlanNode::Join` (see notes)
- [x] Extend `QueryPlanner`/`QueryExecutor` with `GraphMatch` (graph traversal) and `Fusion` plan nodes
- [x] Unit tests + doctests for full hybrid queries
- [x] Rustdoc
- [x] `cargo fmt` / `clippy` clean
- [x] `cargo deny check` clean

### Phase 3 wrap-up

- [x] **Milestone check:** can traverse graphs and perform hybrid scalar+vector+graph queries — covered by `crates/tpt-search-query/tests/graph.rs`, which fuses a `GRAPH(...)` input with scalar and vector inputs

---

## Phase 4 — Distributed Layer (Months 13-18)

*Deliverables: `tpt-search-distributed` (openraft-backed consensus per deviation #3),
shard management, leader-follower replication, query routing and scatter-gather,
cluster management API.*
*Milestone: can run a 3-node cluster with automatic failover and consistent reads.*

**Phase 4 decisions and status** (recorded as built):
- **Consensus is in-house, not openraft.** openraft 0.10 is MIT OR Apache-2.0, but four of
  its dependencies are Apache-2.0-only (`anyerror`, `backoff-series`, `base2histogram`,
  `display-more`), which `deny.toml` and spec.txt reject. The alternative checked was
  `async-raft` 0.6.1. Its tree passes `deny.toml`, but it is unmaintained (last release
  0.6.1), and it failed in testing: a follower reported a read against a committed write
  that its state machine had not applied. The root cause was not isolated inside the
  library. Rather than depend on it, the Raft protocol was written in-house
  (`consensus/node.rs`), and the default dependency graph passes `cargo deny`.
  deviation #3 ("wrap openraft") is superseded by this decision.
- **Raft as implemented:** leader election with randomized timeouts; log replication
  with consistency checks and backoff; commit only for entries of the leader's term; a
  no-op entry at the start of each term; ReadIndex linearizable reads (a leader confirms
  a majority before answering, and a follower waits to apply up to the index it was
  given); one-voter-at-a-time membership changes, with new members catching up as
  learners first.
- **Sharding and replication are connected.** `ShardedCluster` runs one Raft group per
  shard, placed by `ShardManager`. Writes and reads go to the owning shard. Searches run on
  every shard and merge by score. A failed shard leader does not stop the other shards.
- **Limits, all deliberate for this version:**
  - Nothing is persisted. A restarted node has lost its term and vote, which breaks
    Raft's safety rules, so there is no restart, only stop. Durability needs
    `tpt-search-storage` wired in as the log and hard-state store.
  - The log is never compacted and there are no snapshots. A new member replays the full
    log from index 1.
  - The transport is in-process. A stopped node fails calls after a short delay, which
    stands in for a timeout on a real network.
  - `ShardManager::rebalance` reports migrations, but no data is copied. Moving a shard
    to a new replica set is not implemented.
  - Range sharding exists in the placement code but has no shard-level tests beyond
    routing.
- Read and write retries run against a 10-second budget. A client that gets no leader in
  that time receives an error.

**Phase 4 test coverage:**
- `tpt-search-distributed` unit tests and doctest: 14 unit tests (placement, migrations,
  failover in scatter-gather, load balancing) and 1 doctest.
- `tests/cluster.rs` (6): replication with linearizable reads on every node, deletes,
  schema rejections agreed by every replica, leader failover, refusal to commit without
  quorum, and add/remove node with catch-up.
- `tests/consistency.rs` (4): acknowledged writes visible on every survivor right after
  failover; two successive leader failures in five nodes; concurrent writers with a
  leader killed mid-flight.
- `tests/sharded.rs` (2): documents routed to one shard each, cross-shard search, and a
  leader failure in one shard leaving the others serving.
- Stress: the consensus tests passed 10 full runs in a row, and the concurrent-writer
  test passed 20 runs in a row.

### tpt-search-distributed

- [x] Scaffold `crates/tpt-search-distributed/`
- [ ] Wire deps: `tpt-search-core`, `tpt-search-storage`, `openraft` — `tpt-search-core` and `tpt-search-index` wired; `tpt-search-storage` not used yet (no durability); `openraft` replaced by an in-house Raft (see decisions)
- [x] Implement `ShardManager`, `ShardInfo`, `ShardingStrategy` (Hash / Range; Custom not built)
- [x] Implement `route_document` / `route_query` / `rebalance` (+ `ShardMigration`; migrations are reported, not copied)
- [x] Consensus layer: in-house Raft (`consensus/node.rs`), replacing the openraft wrapper of deviation #3
- [x] Leader-follower replication on the Raft log, one Raft group per shard (`ShardedCluster`)
- [x] Implement `QueryRouter` (`route_document` / `scatter_gather`, with failover between replicas), `LoadBalancer` (RoundRobin / LeastConnections / Random / Weighted)
- [x] Cluster management API surface (start, initialize, add/remove node, stop, status, health via `has_quorum`)
- [x] Unit tests + doctests
- [x] Integration test: 3-node cluster, kill the leader mid-test, verify automatic failover and consistent reads afterward
- [x] Rustdoc
- [x] `cargo fmt` / `clippy` clean (workspace, including `--all-features`)
- [x] `cargo deny check` clean (the default dependency graph, which now includes the in-house Raft and no openraft)

### Phase 4 wrap-up

- [x] **Milestone check:** 3-node cluster with automatic failover and consistent reads (in-process, in-memory; durability is a follow-up)

---

## Phase 5 — Edge-to-Cloud Sync (Months 19-22)

*Deliverables: `tpt-search-sync` with CRDT-based conflict resolution, delta
synchronization, offline queue with replay, bandwidth-aware sync, sync monitoring.*
*Milestone: can sync data between edge devices and cloud with offline capability
and automatic conflict resolution.*

**Phase 5 decisions and status** (recorded as built):
- **Conflicts are resolved per field, by last writer wins.** Stamps are Lamport clocks
  with the replica id as tie-breaker, so every replica picks the same winner. Writes to
  different fields never conflict. Deletes are stamped too, so a later edit revives a
  document.
- **Conflict counts are observations, not events.** `SyncReport::conflicts` counts an
  incoming write from another replica that disagrees with the local value of the same
  field. Each replica that applies one side of a conflicting pair observes it, so one
  conflict can count on several replicas. The monitor reports the sum.
- **Offline queue is the WAL.** Each local entry and each upstream acknowledgement is
  written to a `tpt-search-storage` WAL before it counts. This needed a new
  `WalEntry::Opaque` variant in storage, which is the only change to that crate.
  Acknowledged entries are not removed from the log. It keeps full history, so the log
  grows without bound until compaction exists.
- **Sync is delta-based.** Version vectors give each replica's highest applied sequence
  number. A peer receives exactly the entries its version vector lacks. Entries from one
  replica are applied in sequence order. A gap that never fills is reported as an error,
  not applied.
- **Bandwidth.** The planner groups entries by replica and sends whole prefixes, so a
  replica's later entries wait behind any entry that does not fit. Replicas with
  priority-field or delete entries go first. Compression is DEFLATE through `flate2`.
  The caller supplies link state. The crate does not measure it.
- **Standalone CRDTs.** `GCounter`, `PnCounter`, and `OrSet` are implemented and tested,
  but the sync engine replicates only `LwwDocument`. Counter and set fields are not yet
  replicated.
- **Not built:** causal ordering beyond Lamport clocks (concurrency is inferred from
  replica disagreement, not from vector clocks), log compaction, and multi-upstream
  acknowledgement.

**Phase 5 test coverage:** 15 unit tests and 1 doctest (CRDT laws, delta selection,
gap handling, bandwidth planning, compression, and a conflict case); 3 integration tests
(an offline cycle with a restart that keeps the queue, a conflicting edit, convergence,
and monitor totals; a budgeted upload that defers and then drains; deletes and revival);
and 1 randomized convergence test that runs 60 seeds of random edits and exchanges across
three replicas. The `sync_report` example prints the CLI report.

### tpt-search-sync

- [x] Scaffold `crates/tpt-search-sync/`
- [x] Wire deps: `tpt-search-core`, `tpt-search-storage` (WAL for the offline queue)
- [x] Implement `LwwDocument` (+ `merge`), `OrSet`, `GCounter`, `PnCounter` (`CrdtDocument` trait; `CrdtDocument` is the trait, `OrSetDocument` is named `OrSet`)
- [x] Implement `SyncEngine` (`exchange` / `delta_for` / `apply_delta` / `upload` / `download`; conflict resolution is last-writer-wins inside `LwwDocument::merge`, with no separate `resolve_conflicts` step)
- [x] Implement `SyncEntry`, `Change` (the operation), `SyncReport`; the log is an internal `Vec<SyncEntry>`, not a public `SyncLog` type
- [x] Offline queue with replay (persisted via `tpt-search-storage`'s WAL; `SyncEngine::open` replays it)
- [x] Implement `BandwidthManager` (`should_sync` / `compress` / `decompress` / `plan` / `priority`), `SyncPriority`, `LinkState`
- [x] Minimal sync-monitoring surface: `SyncMonitor::render` and the `sync_report` example (applied/conflicts/duplicates/deferred/bytes/errors)
- [x] Unit tests + doctests
- [x] Integration test: an offline/online sync cycle with conflicting edits, verifying automatic conflict resolution (`tests/offline_sync.rs`)
- [x] Rustdoc
- [x] `cargo fmt` / `clippy` clean
- [x] `cargo deny check` clean

### Phase 5 wrap-up

- [x] **Milestone check:** sync between edge devices and cloud with offline capability and automatic conflict resolution

---

## Phase 6 — API & Polish (Months 23-26)

*Deliverables: `tpt-search-api` with REST + gRPC, WebSocket subscriptions,
cost-based query planner, docs/examples, performance tuning and production hardening.*
*Milestone: production-ready search engine with full API surface.*

### tpt-search-api

- [x] Scaffold `crates/tpt-search-api/`
- [~] Wire deps. Built on `hyper` 1 directly, not `axum`/`tonic`/`async-graphql`: all three
      fail `deny.toml` (sync_wrapper, ryu via serde_urlencoded, MPL-2.0 and Apache-only
      transitive crates). `tpt-search-distributed` is not wired in yet.
- [x] Implement REST routes: create/list/drop index, schema, upsert/get/delete document,
      search, streamed (NDJSON) search. The `/graphql` route is blocked by the license policy.
- [x] Implement request/response types (`SearchRequest` shape, highlight spans in hits)
- [ ] gRPC `SearchService` — blocked: `tonic` fails the license policy
- [x] WebSocket real-time subscriptions (hand-rolled RFC 6455 handshake and text frames)
- [x] Schema and index-lifecycle endpoints (`PUT`/`GET`/`DELETE /indexes/{name}`)
- [x] Unit tests (`convert`, `ws`) and integration tests (`tests/api.rs`, 7 tests) for the
      HTTP and WebSocket surface. gRPC tests are not applicable.
- [x] Rustdoc
- [x] `cargo fmt` / `clippy` clean
- [x] `cargo deny check licenses` clean

### tpt-search-query (cost-based optimizer)

- [~] Implement `IndexStatistics` collection. No separate type: the optimizer reads the
      live posting-list and cardinality estimates from the collection, so statistics follow
      the data without a refresh step.
- [x] Implement `QueryPlanner.optimize()` — reorders intersections by estimated cardinality,
      short-circuits empty branches, flattens and de-duplicates unions, drops no-op differences
- [x] Implement `QueryExecutor.execute_streaming()` (batches, zero batch size handled)
- [x] Unit tests for optimizer decisions (`tests/optimizer.rs`, 5 tests). Doctests not added.
- [x] Rustdoc
- [x] `cargo fmt` / `clippy` clean
- [x] `cargo deny check licenses` clean

### Polish

- [~] Comprehensive documentation pass (rustdoc + `examples/` across all crates). Rustdoc
      builds with `-D warnings`; examples exist for api, sync and vector; not every crate has one.
- [~] Performance tuning pass. Hot paths tuned (SIMD distance kernels, monomorphic loops, HNSW
      heuristic). Uniform-random 128-d recall is still open; see the Phase 2 notes.
- [x] Production hardening: query timeouts and candidate limits, request body limits with
      drain, query-length and depth limits in the parser, graceful shutdown via watch channel
- [x] Load/perf testing harness (`tpt-search-api/examples/load.rs`, criterion benches)

### Phase 6 wrap-up

- [ ] **Milestone check:** production-ready search engine with full API surface

---

## Phase 7 — Bindings & Ecosystem (Months 27-30)

*Deliverables: C API, Python bindings, Go bindings, vertical integration examples,
community outreach and adoption.*
*Milestone: drop-in replacement for Elasticsearch in Rust, Python, Go, and C++ ecosystems.*

- [x] Scaffold `crates/tpt-search-capi/` — C ABI as a `cdylib`. Written by hand, not via
      `cbindgen`: its MPL-2.0 build dependencies fail `deny.toml`. The header is
      `include/tpt_search.h`.
- [x] Rust tests for the C API (`tests/capi.rs`, 11 tests) and rustdoc. Clippy and
      `cargo deny` clean. The C header is not compiled here: no C compiler is installed.
- [x] Python bindings: `crates/tpt-search-py/` (PyO3 0.25, module `tpt_search`, maturin
      `pyproject.toml`). `Engine` plus typed exceptions. The license exception for
      `target-lexicon` (build-time dependency of pyo3-build-config) is scoped in `deny.toml`,
      approved by the maintainer.
- [x] Python tests: `tests/test_engine.py` (17 tests) pass against the wheel installed in a
      fresh virtualenv. `python/test_tpt_search.py` in the C API crate (11 tests) drives the C ABI through `ctypes`.
- [ ] Python packaging: wheel builds locally (`maturin build`). Publishing to PyPI is not
      done and needs confirmation.
- [x] Go bindings: `bindings/go/` (package `tpt`, cgo over `tpt-search-capi`). `Engine` with
      typed sentinel errors; thread-local error read on the locked OS thread.
- [x] Go tests: `tpt_test.go` (9 tests, including concurrent searches), passing under `-race`
      with MinGW gcc and the `x86_64-pc-windows-gnu` build of the library. Build steps are in `bindings/go/README.md`.
- [ ] Go example usage (`example_test.go`) and a non-Windows run; only Windows has been exercised.
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
