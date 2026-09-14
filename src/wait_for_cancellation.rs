use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use pin_project_lite::pin_project;
use tokio::sync::futures::Notified;

use crate::CancellationToken;

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

impl<'a> WaitForCancellationFuture<'a> {
    #[inline]
    pub(crate) fn new(token: &'a CancellationToken, notified: Notified<'a>) -> Self {
        Self { token, notified }
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
