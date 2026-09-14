# thin-cancellation-token

A minimal async cancellation token.

Benchmarked on Linux x86_64 with an i9-13900K, Rust 1.94.0, and Tokio 1.53.1, using a release build.

Ranges show the point estimates from two runs, with the implementations tested in opposite orders.
The ratio is tokio-util time / thin time; values above 1 mean thin is faster.

| Operation / scenario | thin | tokio-util 0.7.18 | Time ratio (tokio-util / thin) |
| --- | ---: | ---: | ---: |
| Status check (not cancelled) | 1.07–1.08 ns | 7.72–7.84 ns | 7.19–7.28× |
| Status check (cancelled) | 1.08–1.10 ns | 7.79–7.81 ns | 7.13–7.20× |
| Create and drop | 8.09–8.10 ns | 34.61–35.29 ns | 4.27–4.36× |
| Clone and drop | 9.96–10.03 ns | 23.86–24.27 ns | 2.40–2.42× |
| First cancellation (no waiters) | 15.38–15.79 ns | 21.43–21.74 ns | 1.38–1.39× |
| Repeated cancellation | 4.27–4.36 ns | 9.54–9.70 ns | 2.22–2.24× |
| Create and drop a cancellation future | 6.26–6.38 ns | 6.53–6.55 ns | 1.02–1.05× |
| Wait on a cancelled token | 3.39–3.45 ns | 11.80–11.94 ns | 3.46–3.48× |
| Register and remove a waiter | 31.46–32.03 ns | 40.51–41.20 ns | 1.29× |
| Poll again while pending | 9.96–10.28 ns | 18.28–18.78 ns | 1.83–1.84× |
| Poll with a new waker | 17.36–17.74 ns | 25.34–26.34 ns | 1.46–1.49× |
| Notify 1,024 waiters | 10.03–10.20 µs | 9.92–10.05 µs | 0.97–1.00× |
| Shared status checks: 8 threads, 800,000 per batch | 0.22–0.31 ms | 47.20–54.66 ms | 176.38–218.96× |
| Tokio, single thread: 1,024 tasks completed | 122.39–122.89 µs | 139.41–141.45 µs | 1.14–1.15× |
| Tokio, 4 workers: 1 task completed | 13.17–14.14 µs | 13.11–13.60 µs | 0.96–1.00× |
| Tokio, 4 workers: 1,024 tasks completed | 149.58–215.31 µs | 189.76–283.64 µs | 0.88–1.90× |
| Token handle | 8 B | 8 B | — |
| Heap allocation request (new token) | 48 B | 112 B | — |
| Borrowed cancellation future | 72 B | 72 B | — |
| `async move { token.cancelled().await }` | 88 B | 88 B | — |
| DropGuard / DropGuardRef (each) | 8 B | 8 B | — |
