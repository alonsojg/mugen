"""End-to-end FD discovery timing: in-memory DataFrame -> minimal FDs.

Compares pandas + Desbordante Pyro (single-threaded) with polars + the
``mugen`` extension (Pyro over Arrow, with 1 thread and with all threads).

Each side runs in its own subprocess so that peak RSS is per side and a
timeout kills only that side. A side that exceeds ``--timeout`` is recorded
as ``timeout``. The CSV read happens outside the timer. Both sides are timed
from an in-memory frame, so the timers cover only the handoff and discovery.

Usage:
    .venv/bin/python harness/pipeline_bench.py --rounds 3 --timeout 900 \
        --datasets cargo-subset14 cargo-subset20 --out /tmp/pipeline.json
"""

from __future__ import annotations

import argparse
import json
import resource
import subprocess
import sys
import time
from pathlib import Path

from desbordante_harness import DATASETS, REPO_ROOT

SIDES = ("desbordante", "mugen-1t", "mugen-all")


def _peak_rss_mb() -> float:
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024.0


def worker(side: str, dataset: str, rounds: int) -> dict:
    spec = DATASETS[dataset]
    path = REPO_ROOT / spec.path
    best = float("inf")
    count = -1

    if side == "desbordante":
        import desbordante
        import pandas as pd

        frame = pd.read_csv(path, sep=spec.separator)
        for _ in range(rounds):
            algo = desbordante.fd.algorithms.Pyro()
            start = time.perf_counter()
            algo.load_data(table=frame)
            algo.execute(error=0.0, seed=0, threads=1)
            best = min(best, time.perf_counter() - start)
            count = len(algo.get_fds())
    else:
        import mugen
        import polars as pl

        frame = pl.read_csv(path, separator=spec.separator)
        threads = 1 if side == "mugen-1t" else None
        for _ in range(rounds):
            start = time.perf_counter()
            _, fds = mugen.discover_fds(frame, threads=threads)
            best = min(best, time.perf_counter() - start)
            count = len(fds)

    return {"best_s": best, "fds": count, "peak_rss_mb": _peak_rss_mb()}


def run_side(side: str, dataset: str, rounds: int, timeout: int) -> dict:
    cmd = [
        sys.executable,
        __file__,
        "--worker",
        side,
        "--dataset",
        dataset,
        "--rounds",
        str(rounds),
    ]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {"status": "timeout", "timeout_s": timeout}
    if proc.returncode != 0:
        return {"status": "error", "stderr": proc.stderr[-2000:]}
    return {"status": "ok", **json.loads(proc.stdout.strip().splitlines()[-1])}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--worker", choices=SIDES, help=argparse.SUPPRESS)
    parser.add_argument("--dataset", help=argparse.SUPPRESS)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=900, help="seconds per side")
    parser.add_argument("--datasets", nargs="+", default=["cargo-subset14"])
    parser.add_argument("--sides", nargs="+", choices=SIDES, default=list(SIDES))
    parser.add_argument("--out", type=Path)
    args = parser.parse_args(argv)

    if args.worker:
        print(json.dumps(worker(args.worker, args.dataset, args.rounds)))
        return 0

    results: dict[str, dict] = {}
    for dataset in args.datasets:
        results[dataset] = {}
        for side in args.sides:
            print(f"running {side} on {dataset}...", file=sys.stderr, flush=True)
            results[dataset][side] = run_side(side, dataset, args.rounds, args.timeout)
            print(f"  {results[dataset][side]}", file=sys.stderr, flush=True)

    text = json.dumps(results, indent=2)
    if args.out:
        args.out.write_text(text + "\n")
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
