use std::{
    future::Future,
    pin::pin,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Wake, Waker},
    thread,
};

use thin_cancellation_token::{CancellationToken, WaitForCancellationFuture};

#[derive(Default)]
struct WakeCount(AtomicUsize);

impl Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn counter() -> (Arc<WakeCount>, Waker) {
    let count = Arc::new(WakeCount::default());
    (count.clone(), Waker::from(count))
}

#[test]
fn clones_share_cancellation_but_new_tokens_are_independent() {
    let token = CancellationToken::new();
    let clone = token.clone();
    let independent = CancellationToken::default();
    assert!(!token.is_cancelled());
    clone.cancel();
    assert!(token.is_cancelled());
    assert!(clone.is_cancelled());
    assert!(!independent.is_cancelled());
    token.cancel();
    assert!(clone.is_cancelled());
}

#[test]
fn dropping_a_clone_does_not_cancel() {
    let token = CancellationToken::new();
    drop(token.clone());
    assert!(!token.is_cancelled());
}

#[test]
fn cancellation_before_first_poll_and_late_waiters_are_ready() {
    let token = CancellationToken::new();
    let mut earlier = pin!(token.cancelled());
    token.cancel();
    let mut cx = Context::from_waker(Waker::noop());
    assert!(earlier.as_mut().poll(&mut cx).is_ready());
    assert!(pin!(token.cancelled()).poll(&mut cx).is_ready());
}

#[test]
fn all_registered_waiters_are_woken() {
    let token = CancellationToken::new();
    let (count, waker) = counter();
    let mut cx = Context::from_waker(&waker);
    let mut waits: Vec<_> = (0..16).map(|_| Box::pin(token.cancelled())).collect();
    for wait in &mut waits {
        assert!(wait.as_mut().poll(&mut cx).is_pending());
    }
    token.cancel();
    assert_eq!(count.0.load(Ordering::Relaxed), waits.len());
    token.cancel();
    assert_eq!(count.0.load(Ordering::Relaxed), waits.len());
    for wait in &mut waits {
        assert!(wait.as_mut().poll(&mut cx).is_ready());
    }
}

#[test]
fn dropping_a_waiter_preserves_the_token_and_other_waiters() {
    let token = CancellationToken::new();
    let (dropped_count, dropped_waker) = counter();
    let (kept_count, kept_waker) = counter();
    let mut dropped = Box::pin(token.cancelled());
    let mut kept = pin!(token.cancelled());
    assert!(
        dropped
            .as_mut()
            .poll(&mut Context::from_waker(&dropped_waker))
            .is_pending()
    );
    assert!(
        kept.as_mut()
            .poll(&mut Context::from_waker(&kept_waker))
            .is_pending()
    );
    drop(dropped);
    assert!(!token.is_cancelled());
    token.cancel();
    assert_eq!(dropped_count.0.load(Ordering::Relaxed), 0);
    assert_eq!(kept_count.0.load(Ordering::Relaxed), 1);
    assert!(
        kept.as_mut()
            .poll(&mut Context::from_waker(&kept_waker))
            .is_ready()
    );
}

#[test]
fn cancellation_from_another_thread_wakes_the_waiter() {
    let token = CancellationToken::new();
    let (count, waker) = counter();
    let mut cx = Context::from_waker(&waker);
    let mut wait = pin!(token.cancelled());
    assert!(wait.as_mut().poll(&mut cx).is_pending());
    thread::scope(|scope| {
        scope.spawn(|| token.cancel());
    });
    assert_eq!(count.0.load(Ordering::Relaxed), 1);
    assert!(wait.as_mut().poll(&mut cx).is_ready());
}

#[test]
fn repolling_a_waiter_replaces_its_waker() {
    let token = CancellationToken::new();
    let (old_count, old_waker) = counter();
    let (new_count, new_waker) = counter();
    let mut wait = pin!(token.cancelled());
    assert!(
        wait.as_mut()
            .poll(&mut Context::from_waker(&old_waker))
            .is_pending()
    );
    assert!(
        wait.as_mut()
            .poll(&mut Context::from_waker(&new_waker))
            .is_pending()
    );
    token.cancel();
    assert_eq!(old_count.0.load(Ordering::Relaxed), 0);
    assert_eq!(new_count.0.load(Ordering::Relaxed), 1);
    assert!(
        wait.as_mut()
            .poll(&mut Context::from_waker(&new_waker))
            .is_ready()
    );
}

#[test]
fn wait_future_is_as_compact_as_tokio_util() {
    let token = CancellationToken::new();
    assert_eq!(
        size_of_val(&token.cancelled()),
        size_of::<tokio_util::sync::WaitForCancellationFuture<'_>>(),
        "the wait future must not add an outer async state machine"
    );
}

#[test]
fn cancel_racing_with_first_poll_cannot_lose_a_wakeup() {
    for _ in 0..128 {
        let token = CancellationToken::new();
        let start = Barrier::new(2);
        let (count, waker) = counter();
        let mut cx = Context::from_waker(&waker);
        let mut wait = pin!(token.cancelled());
        let pending = thread::scope(|scope| {
            scope.spawn(|| {
                start.wait();
                token.cancel();
            });
            start.wait();
            wait.as_mut().poll(&mut cx).is_pending()
        });
        if pending {
            assert!(count.0.load(Ordering::Relaxed) > 0);
            assert!(wait.as_mut().poll(&mut cx).is_ready());
        }
        // A Future need not support being polled again after Ready.
        assert!(token.is_cancelled());
    }
}

#[test]
fn concurrent_cancellers_wake_every_waiter() {
    let token = CancellationToken::new();
    let (count, waker) = counter();
    let mut cx = Context::from_waker(&waker);
    let mut waits: Vec<_> = (0..8).map(|_| Box::pin(token.cancelled())).collect();
    for wait in &mut waits {
        assert!(wait.as_mut().poll(&mut cx).is_pending());
    }
    let start = Barrier::new(4);
    thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                start.wait();
                token.cancel();
            });
        }
    });
    assert_eq!(count.0.load(Ordering::Relaxed), waits.len());
    for wait in &mut waits {
        assert!(wait.as_mut().poll(&mut cx).is_ready());
    }
}

#[test]
fn token_is_send_sync_and_wait_future_is_send() {
    fn send_sync<T: Send + Sync>() {}
    fn send(_: impl Send) {}
    send_sync::<CancellationToken>();
    send_sync::<WaitForCancellationFuture<'static>>();
    send(CancellationToken::new().cancelled());
}
