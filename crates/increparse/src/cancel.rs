//! Cooperative cancellation for engine runs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A cheap, thread-safe flag for cancelling an engine run.
///
/// The engine checks the token between batches and executors check it between
/// individual jobs, so cancelling takes effect at job granularity. Clone the
/// token to share it with passes (a pass may cancel a run itself) or with a
/// watchdog thread.
///
/// # Examples
///
/// ```
/// use increparse::CancelToken;
///
/// let token = CancelToken::new();
/// assert!(!token.is_cancelled());
///
/// let shared = token.clone();
/// shared.cancel();
/// assert!(token.is_cancelled());
///
/// token.reset();
/// assert!(!token.is_cancelled());
/// ```
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// Creates a token in the not-cancelled state.
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// Marks the run as cancelled.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Clears the cancelled state so the token can be reused.
    pub fn reset(&self) {
        self.0.store(false, Ordering::Release);
    }

    /// Returns `true` if the token has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
