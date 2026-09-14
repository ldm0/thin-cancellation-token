use std::{
    future::{Future, pending},
    panic::{AssertUnwindSafe, catch_unwind},
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Wake, Waker},
};

use thin_cancellation_token::{CancellationToken, DropGuard, DropGuardRef};

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

#[test]
fn guards_cancel_and_wake_registered_waiters_only_once() {
    let token = CancellationToken::new();
    let owned = token.clone().drop_guard();
    let borrowed = token.drop_guard_ref();
    let count = Arc::new(WakeCount::default());
    let waker = Waker::from(count.clone());
    let mut cx = Context::from_waker(&waker);
    let mut waits: Vec<_> = (0..4).map(|_| Box::pin(token.cancelled())).collect();
    for wait in &mut waits {
        assert!(wait.as_mut().poll(&mut cx).is_pending());
    }
    assert!(!token.is_cancelled());
    drop(owned);
    assert!(token.is_cancelled());
    assert_eq!(count.0.load(Ordering::Relaxed), waits.len());
    drop(borrowed);
    assert_eq!(count.0.load(Ordering::Relaxed), waits.len());
    for wait in &mut waits {
        assert!(wait.as_mut().poll(&mut cx).is_ready());
    }
}

#[test]
fn borrowed_guard_cancels_on_scope_exit() {
    let token = CancellationToken::new();
    {
        let _guard = token.drop_guard_ref();
        assert!(!token.is_cancelled());
    }
    assert!(token.is_cancelled());
    assert!(
        pin!(token.cancelled())
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_ready()
    );
}

#[test]
fn disarming_owned_guard_returns_the_same_live_state() {
    let token = CancellationToken::new();
    let returned = token.clone().drop_guard().disarm();
    assert!(!token.is_cancelled());
    assert!(!returned.is_cancelled());
    returned.cancel();
    assert!(token.is_cancelled());
}

#[test]
fn disarming_borrowed_guard_returns_the_original_borrow() {
    let token = CancellationToken::new();
    let returned = token.drop_guard_ref().disarm();
    assert!(std::ptr::eq(returned, &token));
    assert!(!token.is_cancelled());
}

#[test]
fn disarming_does_not_affect_other_guards_or_undo_cancellation() {
    let token = CancellationToken::new();
    let remaining = token.drop_guard_ref();
    drop(token.clone().drop_guard().disarm());
    token.drop_guard_ref().disarm();
    assert!(!token.is_cancelled());
    drop(remaining);
    assert!(token.clone().drop_guard().disarm().is_cancelled());
    assert!(token.drop_guard_ref().disarm().is_cancelled());
}

#[test]
fn both_guards_cancel_during_panic_unwinding() {
    let owned = CancellationToken::new();
    let borrowed = CancellationToken::new();
    let panic = catch_unwind(AssertUnwindSafe(|| {
        let _owned_guard = owned.clone().drop_guard();
        let _borrowed_guard = borrowed.drop_guard_ref();
        panic!("exercise scope cleanup");
    }));
    assert!(panic.is_err());
    assert!(owned.is_cancelled());
    assert!(borrowed.is_cancelled());
}

#[test]
fn dropping_a_future_holding_a_guard_cancels_even_before_first_poll() {
    for poll_first in [false, true] {
        let token = CancellationToken::new();
        let guard = token.clone().drop_guard();
        let mut task = Box::pin(async move {
            let _guard = guard;
            pending::<()>().await;
        });
        if poll_first {
            assert!(
                task.as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
        }
        assert!(!token.is_cancelled());
        drop(task);
        assert!(token.is_cancelled());
    }
}

#[test]
fn guards_are_send_and_sync() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<DropGuard>();
    send_sync::<DropGuardRef<'static>>();
}
