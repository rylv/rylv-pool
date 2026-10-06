use std::{
    any::Any,
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Mutex,
};

/// A factory failure or an unwinding panic during a pool operation.
#[derive(Debug)]
pub enum PoolError<E> {
    /// The factory returned an error without panicking.
    Factory(E),
    /// The operation panicked and its payload was captured.
    Panic(PanicError),
}

impl<E: fmt::Display> fmt::Display for PoolError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Factory(error) => write!(f, "pool factory failed: {error}"),
            Self::Panic(error) => write!(f, "pool operation panicked: {error}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for PoolError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Factory(error) => Some(error),
            Self::Panic(error) => Some(error),
        }
    }
}

impl<E> PoolError<E> {
    /// Run an operation, preserving factory errors and capturing unwind panics.
    ///
    /// The operation must leave any externally observable state valid when it
    /// unwinds. Panics configured to abort and panics in the panic hook cannot
    /// be captured.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Factory`] for a returned error or [`Self::Panic`] for an
    /// unwinding panic. Successful work completed before a panic is retained.
    pub fn catch<T>(operation: impl FnOnce() -> Result<T, E>) -> Result<T, Self> {
        catch_operation(operation)
            .map_err(Self::Panic)?
            .map_err(Self::Factory)
    }
}

/// The original payload of a captured unwind panic.
///
/// A mutex allows this error to be shared even when the payload is only Send.
pub struct PanicError {
    payload: Mutex<Box<dyn Any + Send>>,
}

impl PanicError {
    /// Recover the original payload for inspection by the caller.
    #[must_use]
    pub fn into_payload(self) -> Box<dyn Any + Send> {
        self.payload
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl fmt::Display for PanicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let payload = self
            .payload
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(message) = payload.downcast_ref::<String>() {
            f.write_str(message)
        } else if let Some(message) = payload.downcast_ref::<&str>() {
            f.write_str(message)
        } else {
            f.write_str("non-string panic payload")
        }
    }
}

impl fmt::Debug for PanicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PanicError")
            .field("message", &format_args!("{self}"))
            .finish_non_exhaustive()
    }
}

impl std::error::Error for PanicError {}

// Keep this helper internal even if its containing module becomes public.
#[allow(clippy::redundant_pub_crate)]
pub(super) fn catch_operation<T>(operation: impl FnOnce() -> T) -> Result<T, PanicError> {
    // Providers run client code outside storage borrows and must preserve
    // their invariants on unwind. Callers own mutable factories exclusively
    // for the operation; a panicking factory is never retried here.
    catch_unwind(AssertUnwindSafe(operation)).map_err(|payload| PanicError {
        payload: Mutex::new(payload),
    })
}

#[cfg(test)]
mod tests;
