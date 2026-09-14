#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

mod cancellation_token;
mod drop_guard;
mod wait_for_cancellation;

pub use cancellation_token::CancellationToken;
pub use drop_guard::{DropGuard, DropGuardRef};
pub use wait_for_cancellation::WaitForCancellationFuture;
