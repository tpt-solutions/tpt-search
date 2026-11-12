//! Logging and metrics setup.
//!
//! Library crates emit `tracing` events and spans; applications decide where they
//! go. [`init_tracing`] is a convenience for binaries, tests, and examples.

use tracing::Level;

/// Installs a global `tracing` subscriber that writes to stderr at `max_level`.
///
/// Returns `false` if a global subscriber was already installed (for example by
/// another call or by the host application). This never panics.
pub fn init_tracing(max_level: Level) -> bool {
    tracing_subscriber::fmt()
        .with_max_level(max_level)
        .with_writer(std::io::stderr)
        .try_init()
        .is_ok()
}
