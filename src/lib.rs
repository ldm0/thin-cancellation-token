#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll},
};

use pin_project_lite::pin_project;
use tokio::sync::{Notify, futures::Notified};
use triomphe::Arc;

/// A one-shot cancellation signal shared by all clones.
///
/// Dropping a token does not cancel it. There are no child tokens or resets.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    inner: Arc<State>,
}

pin_project! {
    /// A future that completes when its borrowed token is cancelled.
    ///
    /// Created by [`CancellationToken::cancelled`]. Dropping this future only
    /// unregisters its waiter; it does not cancel the token.
    #[derive(Debug)]
    #[must_use = "futures do nothing unless polled or awaited"]
    pub struct WaitForCancellationFuture<'a> {
        token: &'a CancellationToken,
        #[pin]
        notified: Notified<'a>,
    }
}

/// An owned guard that cancels its token when dropped, unless disarmed.
///
/// Created by [`CancellationToken::drop_guard`]. Bind the guard to a variable
/// to keep it alive until the end of a scope. This also works during panic
/// unwinding and when a future holding the guard is dropped.
#[derive(Debug)]
#[must_use = "dropping the guard immediately cancels the token"]
pub struct DropGuard {
    token: Option<CancellationToken>,
}

/// A borrowed guard that cancels its token when dropped, unless disarmed.
///
/// Created by [`CancellationToken::drop_guard_ref`]. It does not clone the token
/// or change its reference count, and cannot outlive the borrowed token.
#[derive(Debug)]
#[must_use = "dropping the guard immediately cancels the token"]
pub struct DropGuardRef<'a> {
    token: Option<&'a CancellationToken>,
}

#[derive(Debug, Default)]
struct State {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancellationToken {
    /// Creates an independent, uncancelled token. Equivalent to `default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation and wakes all current waiters.
    ///
    /// Cancellation is permanent. Calling this more than once is harmless.
    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::AcqRel) {
            self.inner.notify.notify_waiters();
        }
    }

    /// Checks cancellation using a single atomic load, without locking.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    /// Takes ownership of this token and cancels it when the guard is dropped.
    ///
    /// Creating a guard does not allocate or clone the token. Call
    /// [`DropGuard::disarm`] to recover the token without cancelling it.
    /// Other guards for the same cancellation state are unaffected.
    #[inline]
    pub fn drop_guard(self) -> DropGuard {
        DropGuard { token: Some(self) }
    }

    /// Borrows this token and cancels it when the guard is dropped.
    ///
    /// This does not allocate or change the token's reference count. Call
    /// [`DropGuardRef::disarm`] to remove the guard without cancelling it.
    #[inline]
    pub fn drop_guard_ref(&self) -> DropGuardRef<'_> {
        DropGuardRef { token: Some(self) }
    }

    /// Waits for cancellation, returning immediately if already cancelled.
    ///
    /// Cancel safe: dropping this future only unregisters this waiter.
    pub fn cancelled(&self) -> WaitForCancellationFuture<'_> {
        // notify_waiters() covers futures created before the notification,
        // even if not yet polled. Create first, then check the sticky flag.
        WaitForCancellationFuture {
            token: self,
            notified: self.inner.notify.notified(),
        }
    }
}

impl Future for WaitForCancellationFuture<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        if this.token.is_cancelled() {
            Poll::Ready(())
        } else {
            // Only cancel() notifies, after setting the permanent flag. A
            // notification therefore suffices even if cancellation raced with
            // the flag check above. Projection keeps the !Unpin waiter pinned.
            this.notified.poll(cx)
        }
    }
}

impl DropGuard {
    /// Consumes the guard and returns its token without requesting cancellation.
    ///
    /// This does not undo an earlier cancellation or disarm other guards.
    #[inline]
    pub fn disarm(mut self) -> CancellationToken {
        self.token.take().expect("a live drop guard is armed")
    }
}

impl Drop for DropGuard {
    #[inline]
    fn drop(&mut self) {
        if let Some(token) = &self.token {
            token.cancel();
        }
    }
}

impl<'a> DropGuardRef<'a> {
    /// Consumes the guard and returns its borrow without requesting cancellation.
    ///
    /// This does not undo an earlier cancellation or disarm other guards.
    #[inline]
    pub fn disarm(mut self) -> &'a CancellationToken {
        self.token.take().expect("a live drop guard is armed")
    }
}

impl Drop for DropGuardRef<'_> {
    #[inline]
    fn drop(&mut self) {
        if let Some(token) = self.token {
            token.cancel();
        }
    }
}
