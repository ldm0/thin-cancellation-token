use crate::CancellationToken;

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

impl DropGuard {
    #[inline]
    pub(crate) fn new(token: CancellationToken) -> Self {
        Self { token: Some(token) }
    }

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
    #[inline]
    pub(crate) fn new(token: &'a CancellationToken) -> Self {
        Self { token: Some(token) }
    }

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
