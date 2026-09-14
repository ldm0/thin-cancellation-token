//! Allocation accounting runs in a separate executable so its instrumentation
//! never affects the Criterion timings. All measured scopes are single-threaded.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    future::Future,
    hint::black_box,
    pin::pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
    task::{Context, Waker},
};

mod support;
use support::{Thin, Token, Tokio};

struct CountingAllocator;

static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn allocated(ptr: *mut u8, layout: Layout) {
    if !ptr.is_null() && ENABLED.load(Relaxed) {
        ALLOCATIONS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size(), Relaxed);
        let live = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
        PEAK.fetch_max(live, Relaxed);
    }
}

// SAFETY: every allocation/deallocation is forwarded to System with the exact
// pointer and Layout supplied by the caller. Accounting does not allocate,
// dereference pointers, or change allocation ownership.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid allocation Layout.
        let ptr = unsafe { System.alloc(layout) };
        allocated(ptr, layout);
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid allocation Layout.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        allocated(ptr, layout);
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ENABLED.load(Relaxed) {
            DEALLOCATIONS.fetch_add(1, Relaxed);
            LIVE.fetch_sub(layout.size(), Relaxed);
        }
        // SAFETY: System allocated this pointer; the caller supplies its Layout.
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn measure<T: Token>(case: &str, work: impl FnOnce()) {
    for counter in [&ALLOCATIONS, &DEALLOCATIONS, &BYTES, &LIVE, &PEAK] {
        counter.store(0, Relaxed);
    }
    ENABLED.store(true, Relaxed);
    work();
    ENABLED.store(false, Relaxed);
    assert_eq!(
        LIVE.load(Relaxed),
        0,
        "measurement must drop its allocations"
    );
    println!(
        "{{\"kind\":\"allocation\",\"implementation\":\"{}\",\"case\":\"{}\",\"allocations\":{},\"deallocations\":{},\"allocated_bytes\":{},\"peak_live_bytes\":{}}}",
        T::NAME,
        case,
        ALLOCATIONS.load(Relaxed),
        DEALLOCATIONS.load(Relaxed),
        BYTES.load(Relaxed),
        PEAK.load(Relaxed),
    );
}

fn measurements<T: Token>() {
    let token = T::new();
    let owned = token.clone();
    let owned_wait = async move { owned.cancelled().await };
    println!(
        "{{\"kind\":\"layout\",\"implementation\":\"{}\",\"token_bytes\":{},\"borrowed_future_bytes\":{},\"owned_wrapper_future_bytes\":{}}}",
        T::NAME,
        size_of::<T>(),
        size_of_val(&token.cancelled()),
        size_of_val(&owned_wait),
    );
    let mut cx = Context::from_waker(Waker::noop());
    measure::<T>("new_drop", || drop(black_box(T::new())));
    measure::<T>("clone_drop", || drop(black_box(token.clone())));
    measure::<T>("wait_create_drop", || {
        let _wait = black_box(token.cancelled());
    });
    measure::<T>("register_drop", || {
        let mut wait = pin!(token.cancelled());
        assert!(black_box(wait.as_mut().poll(&mut cx)).is_pending());
    });
    measure::<T>("boxed_register_drop", || {
        let mut wait = Box::pin(token.cancelled());
        assert!(black_box(wait.as_mut().poll(&mut cx)).is_pending());
    });
    measure::<T>("cancel_fresh", || token.cancel());
    assert!(token.is_cancelled());
    measure::<T>("cancel_again", || token.cancel());
    measure::<T>("wait_ready", || {
        assert!(black_box(pin!(token.cancelled()).poll(&mut cx)).is_ready());
    });
    measure::<T>("independent_tokens/1024", || {
        let mut tokens = Vec::with_capacity(1024);
        for _ in 0..1024 {
            tokens.push(T::new());
        }
        drop(black_box(tokens));
    });
    measure::<T>("shared_clones/1024", || {
        let mut tokens = Vec::with_capacity(1024);
        for _ in 0..1024 {
            tokens.push(token.clone());
        }
        drop(black_box(tokens));
    });
}

fn main() {
    measurements::<Thin>();
    measurements::<Tokio>();

    macro_rules! guards {
        ($token:ty, $owned:ty, $borrowed:ty) => {{
            println!(
                "{{\"kind\":\"guard_layout\",\"implementation\":\"{}\",\"owned_guard_bytes\":{},\"borrowed_guard_bytes\":{}}}",
                <$token as Token>::NAME,
                size_of::<$owned>(),
                size_of::<$borrowed>(),
            );
            let token = <$token>::new();
            let mut returned = None;
            measure::<$token>("guard_owned_create_disarm", || {
                returned = Some(black_box(token).drop_guard().disarm());
            });
            let token = returned.unwrap();
            assert!(!token.is_cancelled());
            measure::<$token>("guard_borrowed_create_disarm", || {
                black_box(black_box(&token).drop_guard_ref().disarm());
            });
            assert!(!token.is_cancelled());

            // Keep a clone alive outside each accounting scope so dropping the
            // owned guard cannot free a token allocated before that scope.
            let owned = token.clone();
            measure::<$token>("guard_owned_drop", || {
                drop(black_box(owned).drop_guard());
            });
            assert!(token.is_cancelled());
            let token = <$token>::new();
            measure::<$token>("guard_borrowed_drop", || {
                drop(black_box(&token).drop_guard_ref());
            });
            assert!(token.is_cancelled());
        }};
    }
    guards!(
        Thin,
        thin_cancellation_token::DropGuard,
        thin_cancellation_token::DropGuardRef<'static>
    );
    guards!(
        Tokio,
        tokio_util::sync::DropGuard,
        tokio_util::sync::DropGuardRef<'static>
    );
}
