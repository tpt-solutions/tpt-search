//! Convergence: under random edits and random pairwise exchanges, replicas that have
//! exchanged everything end in the same state.

use tpt_search_core::{DocumentId, FieldValue};
use tpt_search_sync::{BandwidthManager, SyncEngine};

/// A small deterministic generator, so every failure reproduces from its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// The observable state of a replica: each live document with its field values.
type Snapshot = Vec<(DocumentId, Vec<(String, Option<FieldValue>)>)>;

fn snapshot(engine: &SyncEngine) -> Snapshot {
    let mut out = Vec::new();
    for doc in engine.live_documents() {
        let fields = ["a", "b"]
            .iter()
            .map(|f| (f.to_string(), engine.get(&doc, f).cloned()))
            .collect();
        out.push((doc, fields));
    }
    out
}

fn run(seed: u64) {
    let bw = BandwidthManager::default();
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut engines: Vec<SyncEngine> = (1..=3).map(SyncEngine::new).collect();
    let docs: Vec<DocumentId> = (0..3)
        .map(|i| DocumentId::from(format!("doc-{i}")))
        .collect();

    for _ in 0..150 {
        let r = rng.below(10);
        let e = rng.below(3) as usize;
        let doc = &docs[rng.below(3) as usize];
        match r {
            0..=4 => {
                let field = if rng.below(2) == 0 { "a" } else { "b" };
                let value = FieldValue::Integer(rng.below(5) as i64);
                engines[e].set_field(doc, field, value).unwrap();
            }
            5 => {
                let field = if rng.below(2) == 0 { "a" } else { "b" };
                engines[e].clear_field(doc, field).unwrap();
            }
            6 => {
                engines[e].delete(doc).unwrap();
            }
            _ => {
                // Exchange a random pair.
                let x = rng.below(3) as usize;
                let y = (x + 1 + rng.below(2) as usize) % 3;
                let (lo, hi) = if x < y { (x, y) } else { (y, x) };
                let (left, right) = engines.split_at_mut(hi);
                SyncEngine::exchange(&mut left[lo], &mut right[0], &bw).unwrap();
            }
        }
    }

    // Final full exchange: every pair, twice, so transitive deltas reach everyone.
    for _ in 0..2 {
        for x in 0..3 {
            for y in (x + 1)..3 {
                let (left, right) = engines.split_at_mut(y);
                SyncEngine::exchange(&mut left[x], &mut right[0], &bw).unwrap();
            }
        }
    }

    let reference = snapshot(&engines[0]);
    for (i, engine) in engines.iter().enumerate().skip(1) {
        assert_eq!(
            snapshot(engine),
            reference,
            "seed {seed}: replica {} diverged",
            i + 1
        );
    }
}

#[test]
fn random_edits_and_exchanges_converge_across_many_seeds() {
    for seed in 1..=60 {
        run(seed);
    }
}
