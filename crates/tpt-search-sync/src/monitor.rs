//! Sync monitoring: per-exchange reports, running totals, and a plain-text report for the
//! command line. There is no UI framework in this phase.

use std::fmt;

/// What one sync exchange did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Entries newly applied on the receiving side.
    pub applied: usize,
    /// Conflicts observed while applying: an incoming write from another replica that
    /// disagrees with the local value of the same field. Each replica that applies one side of a
    /// conflicting pair observes it. Last-writer-wins decides the value either way.
    pub conflicts: usize,
    /// Entries already held, so skipped.
    pub duplicates: usize,
    /// Entries left in the offline queue because they did not fit the budget.
    pub deferred: usize,
    /// Size on the wire after compression.
    pub bytes_sent: usize,
    /// Problems, such as a sequence gap that could not be filled.
    pub errors: Vec<String>,
}

impl fmt::Display for SyncReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "applied {}, conflicts {}, duplicates {}, deferred {}, bytes {}, errors {}",
            self.applied,
            self.conflicts,
            self.duplicates,
            self.deferred,
            self.bytes_sent,
            self.errors.len()
        )
    }
}

/// Keeps reports from a run and renders them.
#[derive(Clone, Debug, Default)]
pub struct SyncMonitor {
    reports: Vec<(String, SyncReport)>,
}

impl SyncMonitor {
    /// Creates an empty monitor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one exchange under a label such as `edge-1 upload`.
    pub fn record(&mut self, label: impl Into<String>, report: SyncReport) {
        self.reports.push((label.into(), report));
    }

    /// Totals across every recorded exchange.
    pub fn totals(&self) -> SyncReport {
        let mut total = SyncReport::default();
        for (_, r) in &self.reports {
            total.applied += r.applied;
            total.conflicts += r.conflicts;
            total.duplicates += r.duplicates;
            total.deferred += r.deferred;
            total.bytes_sent += r.bytes_sent;
            total.errors.extend(r.errors.iter().cloned());
        }
        total
    }

    /// A plain-text report: one line per exchange, then the totals.
    pub fn render(&self) -> String {
        let mut out = String::from("sync report\n");
        for (label, report) in &self.reports {
            out.push_str(&format!("  {label:<24} {report}\n"));
        }
        out.push_str(&format!("  {:<24} {}\n", "total", self.totals()));
        out
    }
}
