//! Tokio integration helpers.
//!
//! Index and storage work is CPU- and disk-bound, so async callers should run it
//! on tokio's blocking pool rather than on the reactor threads.

use crate::error::SearchError;

/// Runs `f` on tokio's blocking pool and awaits its result.
///
/// A panic inside `f` is reported as an [`std::io::Error`] wrapped in
/// [`SearchError::Io`].
///
/// # Example
///
/// ```
/// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
/// let n = tpt_search_core::runtime::run_blocking(|| Ok(21 * 2)).await.unwrap();
/// assert_eq!(n, 42);
/// # });
/// ```
pub async fn run_blocking<F, T>(f: F) -> Result<T, SearchError>
where
    F: FnOnce() -> Result<T, SearchError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| SearchError::Io(std::io::Error::other(e)))?
}
