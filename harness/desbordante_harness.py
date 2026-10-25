#!/usr/bin/env python3
"""Generate ground-truth rule fixtures (FDs, UCCs, INDs) with Desbordante.

For every dataset in DATASETS, each algorithm registered for a rule kind is run
and its output is compared with the others. Any disagreement aborts the run, so
a written fixture always reflects output that several exact algorithms agree on.
One canonical JSON fixture per dataset is written to the output directory.

Usage (from the repository root):
    .venv/bin/python harness/desbordante_harness.py [--datasets NAME ...] [--out-dir DIR] [--threads N]

Fixtures contain no timestamps or timings, so reruns are byte-identical. Per-run
timings are written to <out-dir>/_timings.json.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import sys
import time
from dataclasses import dataclass, field
from importlib import metadata
from pathlib import Path
from typing import Any, Callable

import desbordante

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUT_DIR = Path(__file__).resolve().parent / "fixtures"
SCHEMA_VERSION = 1
UCI_BANK_MARKETING = "https://archive.ics.uci.edu/dataset/222/bank+marketing"


class HarnessError(RuntimeError):
    """Raised when algorithms disagree or Desbordante output is inconsistent."""


@dataclass(frozen=True)
class DatasetSpec:
    name: str
    path: str  # relative to the repository root
    separator: str = ";"
    source: str = UCI_BANK_MARKETING


@dataclass(frozen=True)
class AlgorithmSpec:
    name: str
    cls: type
    options: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class Context:
    columns: list[str]
    table_name: str


DATASETS: dict[str, DatasetSpec] = {
    spec.name: spec
    for spec in [
        DatasetSpec("bank", "bank+marketing/bank/bank.csv"),
        DatasetSpec("bank-full", "bank+marketing/bank/bank-full.csv"),
        DatasetSpec(
            "bank-additional",
            "bank+marketing/bank-additional/bank-additional/bank-additional.csv",
        ),
        DatasetSpec(
            "bank-additional-full",
            "bank+marketing/bank-additional/bank-additional/bank-additional-full.csv",
        ),
        DatasetSpec(
            "cargo",
            "cargo+2000+freight+tracking+and+tracing/c2k_data_comma.csv",
            separator=",",
            source="Cargo 2000 Freight Tracking and Tracing (local copy)",
        ),
        DatasetSpec(
            "cargo-subset14",
            "cargo+2000+freight+tracking+and+tracing/c2k_subset14.csv",
            separator=",",
            source="Cargo 2000, 14 columns cut from c2k_data_comma.csv",
        ),
        DatasetSpec(
            "cargo-subset20",
            "cargo+2000+freight+tracking+and+tracing/c2k_subset20.csv",
            separator=",",
            source="Cargo 2000, 20 columns cut from c2k_data_comma.csv",
        ),
        DatasetSpec(
            "cargo-subset30",
            "cargo+2000+freight+tracking+and+tracing/c2k_subset30.csv",
            separator=",",
            source="Cargo 2000, 30 columns cut from c2k_data_comma.csv",
        ),
    ]
}

# Only exact algorithms are registered. Approximate ones (e.g. EulerFD, Aid,
# Faida) and very slow exact ones (FUN) are deliberately excluded.
# HyFD is excluded because it reports FDs that the data violates on
# bank-additional.csv (9 of 3593, checked with pandas); Pyro, DFD, FDep and
# FastFDs agree on the 3584 valid FDs there.
FD_ALGORITHMS = [
    AlgorithmSpec("Pyro", desbordante.fd.algorithms.Pyro, {"error": 0.0, "seed": 0}),
    AlgorithmSpec("DFD", desbordante.fd.algorithms.DFD),
]
UCC_ALGORITHMS = [
    AlgorithmSpec("HyUCC", desbordante.ucc.algorithms.HyUCC),
    AlgorithmSpec("PyroUCC", desbordante.ucc.algorithms.PyroUCC, {"error": 0.0, "seed": 0}),
]
IND_ALGORITHMS = [
    AlgorithmSpec("Spider", desbordante.ind.algorithms.Spider),
]


def _read_csv_shape(path: Path, separator: str) -> tuple[list[str], int]:
    """Return the header and data row count, checking every row has the same width."""
    with path.open(newline="", encoding="utf-8") as handle:
        reader = csv.reader(handle, delimiter=separator)
        header = next(reader)
        n_rows = 0
        widths = set()
        for row in reader:
            if not row:
                continue
            n_rows += 1
            widths.add(len(row))
    if widths != {len(header)}:
        raise HarnessError(f"{path}: ragged rows (widths {sorted(widths)} vs header {len(header)})")
    if len(set(header)) != len(header):
        raise HarnessError(f"{path}: duplicate column names")
    return header, n_rows


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _run_algorithm(kind: str, spec: AlgorithmSpec, dataset: DatasetSpec, threads: int) -> tuple[Any, float]:
    algo = spec.cls()
    table = (str(REPO_ROOT / dataset.path), dataset.separator, True)
    start = time.perf_counter()
    if kind == "ind":
        algo.load_data(tables=[table])
    else:
        algo.load_data(table=table)
    algo.execute(threads=threads, **spec.options)
    return algo, time.perf_counter() - start


def _canonical_fds(algo: Any, ctx: Context) -> list[dict[str, Any]]:
    records = []
    for fd in algo.get_fds():
        lhs_idx = tuple(sorted(fd.lhs_indices))
        lhs_names = [ctx.columns[i] for i in lhs_idx]
        rhs_name = ctx.columns[fd.rhs_index]
        reported_lhs, reported_rhs = fd.to_name_tuple()
        if list(reported_lhs) != lhs_names or reported_rhs != rhs_name:
            raise HarnessError(f"FD column mapping mismatch: {fd.to_long_string()}")
        records.append(((fd.rhs_index, lhs_idx), {"lhs": lhs_names, "rhs": rhs_name}))
    return [record for _, record in sorted(records, key=lambda item: item[0])]


def _canonical_uccs(algo: Any, ctx: Context) -> list[dict[str, Any]]:
    records = []
    for ucc in algo.get_uccs():
        idx = tuple(sorted(ucc.indices))
        names = [ctx.columns[i] for i in idx]
        if ucc.to_long_string() != f"[{' '.join(names)}]":
            raise HarnessError(f"UCC column mapping mismatch: {ucc.to_long_string()}")
        records.append(((len(idx), idx), {"columns": names}))
    return [record for _, record in sorted(records, key=lambda item: item[0])]


def _canonical_inds(algo: Any, ctx: Context) -> list[dict[str, Any]]:
    records = []
    for ind in algo.get_inds():
        if ind.get_error() != 0.0:
            raise HarnessError(f"Spider reported a non-exact IND: {ind.to_long_string()}")
        lhs_table, lhs_idx = ind.get_lhs().to_index_tuple()
        rhs_table, rhs_idx = ind.get_rhs().to_index_tuple()
        if lhs_table != 0 or rhs_table != 0:
            raise HarnessError("IND references an unexpected table index")
        lhs_cols = [ctx.columns[i] for i in lhs_idx]
        rhs_cols = [ctx.columns[i] for i in rhs_idx]
        expected = f"({ctx.table_name}, [{', '.join(lhs_cols)}]) -> ({ctx.table_name}, [{', '.join(rhs_cols)}])"
        if ind.to_long_string() != expected:
            raise HarnessError(f"IND column mapping mismatch: {ind.to_long_string()} vs {expected}")
        record = {
            "lhs": {"table": ctx.table_name, "columns": lhs_cols},
            "rhs": {"table": ctx.table_name, "columns": rhs_cols},
        }
        records.append(((lhs_idx, rhs_idx), record))
    return [record for _, record in sorted(records, key=lambda item: item[0])]


CANONICALIZERS: dict[str, Callable[[Any, Context], list[dict[str, Any]]]] = {
    "fd": _canonical_fds,
    "ucc": _canonical_uccs,
    "ind": _canonical_inds,
}

ALGORITHMS_BY_KIND: dict[str, list[AlgorithmSpec]] = {
    "fd": FD_ALGORITHMS,
    "ucc": UCC_ALGORITHMS,
    "ind": IND_ALGORITHMS,
}


def _discover_kind(
    kind: str,
    dataset: DatasetSpec,
    ctx: Context,
    threads: int,
    timings: dict[str, float],
) -> dict[str, Any]:
    """Run every algorithm for one rule kind and require identical outputs."""
    canonicalize = CANONICALIZERS[kind]
    algorithms = ALGORITHMS_BY_KIND[kind]
    results: dict[str, list[dict[str, Any]]] = {}
    for spec in algorithms:
        algo, elapsed = _run_algorithm(kind, spec, dataset, threads)
        timings[f"{kind}:{spec.name}"] = round(elapsed, 4)
        results[spec.name] = canonicalize(algo, ctx)

    reference_name = algorithms[0].name
    reference = results[reference_name]
    for name, items in results.items():
        if items != reference:
            raise HarnessError(
                f"{dataset.name}/{kind}: {name} ({len(items)} rules) disagrees with "
                f"{reference_name} ({len(reference)} rules)"
            )
    return {
        "algorithms": [spec.name for spec in algorithms],
        "options": {spec.name: spec.options for spec in algorithms},
        "count": len(reference),
        "items": reference,
    }


def build_fixture(dataset: DatasetSpec, threads: int, timings: dict[str, float]) -> dict[str, Any]:
    path = REPO_ROOT / dataset.path
    if not path.is_file():
        raise HarnessError(f"Dataset file not found: {path}")
    columns, n_rows = _read_csv_shape(path, dataset.separator)
    ctx = Context(columns=columns, table_name=path.name)
    rules = {kind: _discover_kind(kind, dataset, ctx, threads, timings) for kind in CANONICALIZERS}
    return {
        "schema_version": SCHEMA_VERSION,
        "dataset": {
            "name": dataset.name,
            "file": dataset.path,
            "source": dataset.source,
            "sha256": _sha256(path),
            "separator": dataset.separator,
            "num_rows": n_rows,
            "num_columns": len(columns),
            "columns": columns,
        },
        "generator": {"tool": "desbordante", "version": metadata.version("desbordante")},
        "rules": rules,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--datasets", nargs="+", choices=sorted(DATASETS), default=sorted(DATASETS))
    parser.add_argument("--out-dir", type=Path, default=DEFAULT_OUT_DIR)
    parser.add_argument("--threads", type=int, default=1, help="threads passed to each Desbordante algorithm")
    args = parser.parse_args(argv)

    args.out_dir.mkdir(parents=True, exist_ok=True)
    all_timings: dict[str, dict[str, float]] = {}
    for name in args.datasets:
        dataset = DATASETS[name]
        print(f"[{name}] discovering rules...", file=sys.stderr)
        timings: dict[str, float] = {}
        fixture = build_fixture(dataset, args.threads, timings)
        out_path = args.out_dir / f"{name}.json"
        out_path.write_text(json.dumps(fixture, indent=2) + "\n", encoding="utf-8")
        all_timings[name] = timings
        counts = {kind: rules["count"] for kind, rules in fixture["rules"].items()}
        print(f"[{name}] wrote {out_path} counts={counts}", file=sys.stderr)

    timings_path = args.out_dir / "_timings.json"
    timings_path.write_text(
        json.dumps({"threads": args.threads, "seconds": all_timings}, indent=2) + "\n", encoding="utf-8"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
