use std::future::Future;

use quill_core::error::Result;

/// A shared, thread-safe connection pool.
///
/// Construction uses concrete driver settings
///
/// ```
/// use std::sync::Arc;
/// use quill_drivers::ConnectionPool;
///
/// fn share<P: ConnectionPool + 'static>(pool: P) -> Arc<dyn ConnectionPool> {
///     Arc::new(pool)
/// }
/// ```
pub trait ConnectionPool: Send + Sync {
    /// Driver-specific parameters used only during construction.
    type Settings
    where
        Self: Sized;

    /// Establish a pool using the concrete driver's settings.
    fn connect(settings: &Self::Settings) -> impl Future<Output = Result<Self>> + Send
    where
        Self: Sized;
}
