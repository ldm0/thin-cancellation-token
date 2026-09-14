use std::{
    future::{Future, poll_fn},
    hint::black_box,
    pin::{Pin, pin},
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use criterion::{
    BatchSize, BenchmarkGroup, Criterion, Throughput, criterion_group, criterion_main,
    measurement::WallTime,
};
use tokio::runtime::{Builder, Runtime};

mod support;
use support::{Thin, Token, Tokio};

// An object-safe query interface lets every implementation use the exact same
// machine-code loop. Separate monomorphized loops can have different alignment,
// which materially changes sub-nanosecond call-throughput measurements.
trait StatusQuery {
    fn status(&self) -> bool;
}

impl<T: Token> StatusQuery for T {
    fn status(&self) -> bool {
        self.is_cancelled()
    }
}

#[inline(never)]
fn query_iterations(token: &dyn StatusQuery, iterations: u64) -> Duration {
    // Hide the concrete receiver and vtable before entering the common loop.
    // The opaque call may have side effects, so it cannot be hoisted/eliminated.
    let token = black_box(token);
    let started = Instant::now();
    for _ in 0..iterations {
        black_box(token.status());
    }
    started.elapsed()
}

fn scalar<T: Token>(group: &mut BenchmarkGroup<'_, WallTime>, case: &str) {
    let token = T::new();
    let mut cx = Context::from_waker(Waker::noop());
    group.bench_function(T::NAME, |b| match case {
        "query_live" => b.iter_custom(|iterations| query_iterations(&token, iterations)),
        "query_cancelled" => {
            token.cancel();
            b.iter_custom(|iterations| query_iterations(&token, iterations));
        }
        "new_drop" => b.iter(|| black_box(T::new())),
        "clone_drop" => b.iter(|| black_box(black_box(&token).clone())),
        "cancel_fresh" => b.iter_batched_ref(
            T::new,
            |token| black_box(token).cancel(),
            BatchSize::NumIterations(64),
        ),
        "cancel_again" => {
            token.cancel();
            b.iter(|| black_box(&token).cancel());
        }
        "wait_ready" => {
            token.cancel();
            b.iter(|| black_box(pin!(black_box(&token).cancelled()).poll(&mut cx)));
        }
        "register_drop" => {
            b.iter(|| black_box(pin!(black_box(&token).cancelled()).poll(&mut cx)));
        }
        "wait_create_drop" => b.iter(|| black_box(black_box(&token).cancelled())),
        "repoll_pending" => {
            let mut wait = pin!(token.cancelled());
            assert!(wait.as_mut().poll(&mut cx).is_pending());
            b.iter(|| black_box(wait.as_mut().poll(&mut cx)));
        }
        "repoll_changed_waker" => {
            let wakers = [
                Waker::from(Arc::new(WakeCount::default())),
                Waker::from(Arc::new(WakeCount::default())),
            ];
            let mut wait = pin!(token.cancelled());
            let mut index = 0;
            assert!(
                wait.as_mut()
                    .poll(&mut Context::from_waker(&wakers[1]))
                    .is_pending()
            );
            b.iter(|| {
                let result = wait.as_mut().poll(&mut Context::from_waker(&wakers[index]));
                index ^= 1;
                black_box(result)
            });
        }
        _ => unreachable!(),
    });
}

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

type Waiter = Pin<Box<dyn Future<Output = ()> + Send>>;

fn waiting<T: Token>(n: usize) -> (T, Vec<Waiter>, Arc<WakeCount>) {
    let token = T::new();
    let count = Arc::new(WakeCount::default());
    let waker = Waker::from(count.clone());
    let mut cx = Context::from_waker(&waker);
    let mut waits: Vec<Waiter> = (0..n)
        .map(|_| {
            let token = token.clone();
            Box::pin(async move { token.cancelled().await }) as Waiter
        })
        .collect();
    for wait in &mut waits {
        assert!(wait.as_mut().poll(&mut cx).is_pending());
    }
    (token, waits, count)
}

fn wake<T: Token>(group: &mut BenchmarkGroup<'_, WallTime>, n: usize) {
    // Validate the fixture before timing. This measures Notify bookkeeping
    // and waker calls, not a runtime's scheduling latency.
    let (token, mut waits, count) = waiting::<T>(n);
    token.cancel();
    assert_eq!(count.0.load(Ordering::Relaxed), n);
    for wait in &mut waits {
        assert!(
            wait.as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_ready()
        );
    }
    group.bench_function(T::NAME, |b| {
        b.iter_batched_ref(
            || waiting::<T>(n),
            |(token, _, _)| token.cancel(),
            BatchSize::NumIterations(64),
        );
    });
}

const READS_PER_THREAD: u64 = 100_000;
const MUTATIONS_PER_THREAD: u64 = 10_000;

#[derive(Clone, Copy)]
enum Operation {
    SharedRead,
    IndependentRead,
    CloneDrop,
    CancelAgain,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::SharedRead => "readers",
            Self::IndependentRead => "independent_readers",
            Self::CloneDrop => "contended_clone_drop",
            Self::CancelAgain => "contended_cancel_again",
        }
    }

    fn iterations(self) -> u64 {
        match self {
            Self::SharedRead | Self::IndependentRead => READS_PER_THREAD,
            Self::CloneDrop | Self::CancelAgain => MUTATIONS_PER_THREAD,
        }
    }
}

struct Workers {
    start: Arc<Barrier>,
    done: Arc<Barrier>,
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

impl Workers {
    fn new<T: Token>(n: usize, operation: Operation) -> Self {
        let token = T::new();
        if matches!(operation, Operation::CancelAgain) {
            token.cancel();
        }
        let mut pool = Self {
            start: Arc::new(Barrier::new(n + 1)),
            done: Arc::new(Barrier::new(n + 1)),
            stop: Arc::new(AtomicBool::new(false)),
            workers: Vec::new(),
        };
        for _ in 0..n {
            let token = if matches!(operation, Operation::IndependentRead) {
                T::new()
            } else {
                token.clone()
            };
            let (start, done, stop) = (pool.start.clone(), pool.done.clone(), pool.stop.clone());
            pool.workers.push(thread::spawn(move || {
                loop {
                    start.wait();
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    // Dispatch once per batch, outside the hot loop.
                    match operation {
                        Operation::SharedRead | Operation::IndependentRead => {
                            for _ in 0..READS_PER_THREAD {
                                black_box(black_box(&token).is_cancelled());
                            }
                        }
                        Operation::CloneDrop => {
                            for _ in 0..MUTATIONS_PER_THREAD {
                                drop(black_box(black_box(&token).clone()));
                            }
                        }
                        Operation::CancelAgain => {
                            for _ in 0..MUTATIONS_PER_THREAD {
                                black_box(&token).cancel();
                            }
                        }
                    }
                    done.wait();
                }
            }));
        }
        pool
    }

    fn round(&self) {
        self.start.wait();
        self.done.wait();
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.start.wait();
        for worker in self.workers.drain(..) {
            worker.join().expect("benchmark worker panicked");
        }
    }
}

fn workers<T: Token>(group: &mut BenchmarkGroup<'_, WallTime>, (n, operation): (usize, Operation)) {
    let pool = Workers::new::<T>(n, operation);
    pool.round();
    group.bench_function(T::NAME, |b| {
        b.iter_custom(|iterations| {
            let started = Instant::now();
            for _ in 0..iterations {
                pool.round();
            }
            started.elapsed()
        });
    });
}

async fn runtime_round<T: Token>(n: usize) -> Duration {
    let token = T::new();
    // Each task reports readiness only AFTER polling its cancellation future
    // to Pending. No sleeps/yields used as a substitute for registration.
    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut tasks = Vec::with_capacity(n);
    for _ in 0..n {
        let token = token.clone();
        let ready_tx = ready_tx.clone();
        tasks.push(tokio::spawn(async move {
            let mut wait = pin!(token.cancelled());
            poll_fn(|cx| {
                assert!(wait.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            ready_tx.send(()).expect("ready receiver dropped");
            wait.await;
        }));
    }
    drop(ready_tx);
    for _ in 0..n {
        ready_rx
            .recv()
            .await
            .expect("waiter exited before registering");
    }
    let started = Instant::now();
    token.cancel();
    for task in tasks {
        task.await.expect("waiter task panicked");
    }
    // Includes cancel(), scheduler work, task completion and JoinHandle awaits.
    // Excludes token/task construction and initial waiter registration.
    started.elapsed()
}

fn runtime_wake<T: Token>(group: &mut BenchmarkGroup<'_, WallTime>, (rt, n): (&Runtime, usize)) {
    rt.block_on(runtime_round::<T>(n));
    group.bench_function(T::NAME, |b| {
        b.iter_custom(|iterations| {
            rt.block_on(async {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    elapsed += runtime_round::<T>(n).await;
                }
                elapsed
            })
        });
    });
}

fn benchmarks(c: &mut Criterion) {
    // Normalize glibc's single-thread optimization before either implementation
    // runs; real Tokio applications also create threads. No startup is timed.
    thread::spawn(|| {}).join().unwrap();
    let reverse = std::env::var_os("THIN_BENCH_REVERSE").is_some();
    eprintln!(
        "order={}, handle bytes: thin={}, tokio_util={}",
        if reverse { "tokio,thin" } else { "thin,tokio" },
        size_of::<Thin>(),
        size_of::<Tokio>(),
    );
    macro_rules! compare {
        ($bench:ident, $group:expr, $arg:expr) => {
            if reverse {
                $bench::<Tokio>($group, $arg);
                $bench::<Thin>($group, $arg);
            } else {
                $bench::<Thin>($group, $arg);
                $bench::<Tokio>($group, $arg);
            }
        };
    }
    for case in [
        "query_live",
        "query_cancelled",
        "new_drop",
        "clone_drop",
        "cancel_fresh",
        "cancel_again",
        "wait_ready",
        "register_drop",
        "wait_create_drop",
        "repoll_pending",
        "repoll_changed_waker",
    ] {
        let mut group = c.benchmark_group(case);
        compare!(scalar, &mut group, case);
        group.finish();
    }
    for n in [1, 8, 64, 1024] {
        let mut group = c.benchmark_group(format!("wake/{n}"));
        compare!(wake, &mut group, n);
        group.finish();
    }
    for operation in [
        Operation::SharedRead,
        Operation::IndependentRead,
        Operation::CloneDrop,
        Operation::CancelAgain,
    ] {
        for n in [1, 2, 4, 8] {
            let mut group = c.benchmark_group(format!("{}/{n}", operation.name()));
            group.throughput(Throughput::Elements(n as u64 * operation.iterations()));
            compare!(workers, &mut group, (n, operation));
            group.finish();
        }
    }
    for name in ["current_thread", "multi_thread_4"] {
        let rt = if name == "current_thread" {
            Builder::new_current_thread().build().unwrap()
        } else {
            Builder::new_multi_thread()
                .worker_threads(4)
                .build()
                .unwrap()
        };
        for n in [1, 64, 1024] {
            let mut group = c.benchmark_group(format!("runtime/{name}/{n}"));
            compare!(runtime_wake, &mut group, (&rt, n));
            group.finish();
        }
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(40)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        .nresamples(10_000);
    targets = benchmarks
}
criterion_main!(benches);
