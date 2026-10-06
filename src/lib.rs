#![doc = include_str!("../README.md")]
// #![deny(unsafe_code)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]
#![deny(clippy::unreachable)]
#![deny(clippy::todo)]
#![deny(clippy::unimplemented)]
#![deny(clippy::pedantic)]
#![deny(clippy::nursery)]
// Disabled because it reports false duplicate-crate errors from dev-dependencies
#![deny(clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]
// Hot-path inlining is intentional.
#![allow(clippy::inline_always)]
#![deny(missing_docs)]
#![deny(clippy::missing_errors_doc)]
#![deny(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]
#![cfg_attr(test, allow(clippy::unreachable))]
#![cfg_attr(test, allow(clippy::unimplemented))]
#![cfg_attr(test, allow(clippy::unwrap_used))]
#![cfg_attr(test, allow(clippy::expect_used))]
#![cfg_attr(test, allow(clippy::panic))]

mod core;
mod error;
mod thread_local;

pub use core::{PoolGuard, PoolItem, PoolProvider};
pub use error::{PanicError, PoolError};
#[doc(hidden)]
pub use thread_local::FixedThreadLocalEntry;
pub use thread_local::{FixedThreadLocalPool, FixedThreadLocalPoolGuard};
