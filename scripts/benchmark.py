#!/usr/bin/env python3
"""Run both benchmark orders and preserve estimates, raw samples and environment."""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]
BUILD_NAMES = {
    "cargo", "rustc", "rustdoc", "rustfmt", "clippy-driver", "cargo-nextest",
    "nextest", "cmake", "ninja", "clang", "clang++", "gcc", "g++", "cc1", "cc1plus",
}
SOURCES = [
    "Cargo.toml", "Cargo.lock",
    *sorted(str(path.relative_to(ROOT)) for path in (ROOT / "src").rglob("*.rs")),
    "benches/cancellation.rs",
    "benches/memory.rs", "benches/support/mod.rs", "scripts/benchmark.py",
    "tests/cancellation.rs", "tests/drop_guard.rs", "README.md",
]
IMPLEMENTATIONS = {
    "thin": "Current CancellationToken using triomphe::Arc",
    "tokio_util": "tokio_util::sync::CancellationToken",
}


def utc():
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds")


def hashes(paths):
    return {str(p): hashlib.sha256((ROOT / p).read_bytes()).hexdigest() for p in paths}


def command(args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def snapshot():
    builds = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            name = (entry / "comm").read_text().strip()
            if name in BUILD_NAMES:
                builds.append({"pid": int(entry.name), "name": name})
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            pass
    return {
        "utc": utc(), "load_average": os.getloadavg(), "build_processes": builds,
        "cpu_ticks": [line for line in Path("/proc/stat").read_text().splitlines()
                      if line.startswith("cpu")],
    }


def collect(criterion_home, baseline):
    measurements = []
    for path in sorted(criterion_home.glob(f"**/{baseline}/benchmark.json")):
        metadata = json.loads(path.read_text())
        estimates = json.loads(path.with_name("estimates.json").read_text())
        sample = json.loads(path.with_name("sample.json").read_text())
        estimate = "slope" if estimates.get("slope") else "mean"
        chosen = estimates[estimate]
        measurements.append({
            "case": metadata["group_id"], "implementation": metadata["function_id"],
            "run": baseline, "estimate": estimate, "time_ns": chosen["point_estimate"],
            "ci95_ns": [chosen["confidence_interval"]["lower_bound"],
                        chosen["confidence_interval"]["upper_bound"]],
            "standard_error_ns": chosen["standard_error"],
            "throughput": metadata["throughput"], "sample_count": len(sample["iters"]),
            "raw_sample": sample, "all_estimates": estimates,
            "tukey_fences": json.loads(path.with_name("tukey.json").read_text()),
        })
    if not measurements:
        raise RuntimeError(f"No samples found for {baseline}")
    return measurements


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cpus", help="Linux taskset CPU list, e.g. 8-15")
    parser.add_argument("--filter", help="Optional Criterion benchmark-name regular expression")
    args = parser.parse_args()
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    run_kind = "filtered" if args.filter else "comprehensive"
    out = ROOT / "target" / "benchmark-results" / f"{run_kind}_{stamp}"
    out.mkdir(parents=True, exist_ok=False)
    print(f"Artifacts: {out}", flush=True)
    protocol_path = ROOT / "README.md"
    (out / "PROTOCOL.md").write_bytes(protocol_path.read_bytes())

    build_args = ["cargo", "bench", "--locked", "--no-run", "--message-format=json"]
    with (out / "build.jsonl").open("w") as stdout, (out / "build.log").open("w") as stderr:
        subprocess.run(build_args, cwd=ROOT, stdout=stdout, stderr=stderr, check=True)
    binaries = {}
    for line in (out / "build.jsonl").read_text().splitlines():
        artifact = json.loads(line)
        if (artifact.get("reason") == "compiler-artifact" and artifact.get("executable")
                and "bench" in artifact["target"]["kind"]):
            binaries[artifact["target"]["name"]] = artifact["executable"]
    # Preserve the exact executables; later Cargo builds may reuse their paths.
    (out / "bin").mkdir()
    for name, executable in binaries.items():
        saved = out / "bin" / name
        shutil.copy2(executable, saved)
        binaries[name] = str(saved)
    prefix = ["taskset", "-c", args.cpus] if args.cpus else []
    selection = [args.filter] if args.filter else []
    with (out / "fixtures.log").open("w") as log:
        subprocess.run(prefix + [binaries["cancellation"], "--test"] + selection, cwd=ROOT,
                       stdout=log, stderr=subprocess.STDOUT, check=True, timeout=60)
    selected = {
        tuple(line.removeprefix("Testing ").rsplit("/", 1))
        for line in (out / "fixtures.log").read_text().splitlines() if line.startswith("Testing ")
    }
    if not selected:
        raise RuntimeError("The benchmark filter selected no fixtures")
    selected_implementations = {implementation for case, implementation in selected}
    forward = [name for name in IMPLEMENTATIONS if name in selected_implementations]
    forward_name = f"{forward[0]}_first"
    orders = {
        forward_name: forward,
        "tokio_first": list(reversed(forward)),
    }
    with (out / "memory.jsonl").open("w") as log:
        subprocess.run([binaries["memory"]], cwd=ROOT, stdout=log, check=True)

    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    versions = {p["name"]: p["version"] for p in lock["package"]
                if p["name"] in {"tokio", "tokio-util", "criterion", "triomphe",
                                 "pin-project-lite", "thin-cancellation-token"}}
    result = {
        "schema_version": 3, "started_utc": utc(), "versions": versions,
        "implementations": {name: IMPLEMENTATIONS[name] for name in forward},
        "source_sha256": hashes(SOURCES),
        "binary_sha256": hashes([binaries["cancellation"], binaries["memory"]]),
        "protocol_snapshot_sha256": hashlib.sha256((out / "PROTOCOL.md").read_bytes()).hexdigest(),
        "environment": {
            "rustc": command(["rustc", "-Vv"]), "cargo": command(["cargo", "-V"]),
            "kernel": platform.platform(), "lscpu": command(["lscpu"]),
            "libc": platform.libc_ver(), "cpu_affinity": args.cpus or sorted(os.sched_getaffinity(0)),
            "cpus_exclusive": False, "profile": "Cargo bench profile; environment flags recorded below",
            "RUSTFLAGS": os.environ.get("RUSTFLAGS"),
            "CARGO_ENCODED_RUSTFLAGS": os.environ.get("CARGO_ENCODED_RUSTFLAGS"),
        },
        "protocol": {
            "cases": len({case for case, implementation in selected}),
            "implementations": len({implementation for case, implementation in selected}),
            "benchmark_filter": args.filter,
            "orders": list(orders),
            "implementation_order": orders,
            "samples_per_case": 40, "warmup_seconds": 0.5, "measurement_seconds": 2,
            "bootstrap_resamples": 10000, "process_monitor_interval_seconds": 2,
            "precreated_thread": True, "allocation_instrumentation_in_timing_binary": False,
            "read_operations_per_thread": 100000, "mutation_operations_per_thread": 10000,
            "runtime_workers": 4, "source_protocol": "README.md",
            "query_loop": "One non-generic, non-inlined function with dynamic dispatch; includes call overhead",
        },
        "memory": [item for line in (out / "memory.jsonl").read_text().splitlines()
                   if (item := json.loads(line))["implementation"] in selected_implementations],
        "artifact_directory": str(out.relative_to(ROOT)), "runs": [], "measurements": [],
    }
    criterion_home = out / "criterion"
    for index, baseline in enumerate(result["protocol"]["orders"]):
        env = os.environ.copy()
        env["CRITERION_HOME"] = str(criterion_home)
        env.pop("THIN_BENCH_REVERSE", None)
        if index == 1:
            env["THIN_BENCH_REVERSE"] = "1"
        run = {"name": baseline, "started_utc": utc(), "snapshots": 0,
               "build_process_records": 0, "snapshots_with_build_processes": 0,
               "build_process_name_records": {}, "maximum_one_minute_load_average": 0}
        cmd = prefix + [binaries["cancellation"], "--bench", "--save-baseline", baseline] + selection
        run["command"] = cmd
        print(f"{run['started_utc']} Starting {baseline}", flush=True)
        with (out / f"{baseline}.log").open("w") as log, (out / f"{baseline}-monitor.jsonl").open("w") as monitor:
            process = subprocess.Popen(cmd, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
            while True:
                state = snapshot()
                monitor.write(json.dumps(state) + "\n")
                monitor.flush()
                run["snapshots"] += 1
                run["build_process_records"] += len(state["build_processes"])
                run["maximum_one_minute_load_average"] = max(
                    run["maximum_one_minute_load_average"], state["load_average"][0])
                if state["build_processes"]:
                    run["snapshots_with_build_processes"] += 1
                    run.setdefault("first_build_observed_utc", state["utc"])
                    run["last_build_observed_utc"] = state["utc"]
                    for item in state["build_processes"]:
                        name = item["name"]
                        counts = run["build_process_name_records"]
                        counts[name] = counts.get(name, 0) + 1
                try:
                    run["exit_code"] = process.wait(timeout=2)
                    break
                except subprocess.TimeoutExpired:
                    pass
        run["finished_utc"] = utc()
        result["runs"].append(run)
        if run["exit_code"] == 0:
            measurements = collect(criterion_home, baseline)
            run["measurements"] = len(measurements)
            result["measurements"].extend(measurements)
            assert {(m["case"], m["implementation"]) for m in measurements} == selected
            assert len({(m["case"], m["implementation"]) for m in measurements}) == len(measurements)
            assert all(m["sample_count"] == 40 for m in measurements)
        (out / "results.json").write_text(json.dumps(result, indent=2) + "\n")
        print(f"{run['finished_utc']} Finished {baseline}: {run}", flush=True)
        if run["exit_code"]:
            raise SystemExit(run["exit_code"])
    result["finished_utc"] = utc()
    result["background_builds_observed"] = any(r["build_process_records"] for r in result["runs"])
    result["sources_unchanged_during_sampling"] = hashes(SOURCES) == result["source_sha256"]
    (out / "results.json").write_text(json.dumps(result, indent=2) + "\n")
    if not result["sources_unchanged_during_sampling"]:
        raise RuntimeError("Measured sources changed during sampling; see preserved artifacts")
    print(f"Completed: {out / 'results.json'}", flush=True)


if __name__ == "__main__":
    main()
