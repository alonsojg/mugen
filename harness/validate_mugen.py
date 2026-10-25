"""Checks that the mugen extension reports the same minimal FDs as Desbordante.

For each Cargo column subset, runs Desbordante Pyro on the CSV and mugen
Pyro on the same data loaded as a pyarrow Table and as a Polars DataFrame.
Compares the FD sets by column name, not by index.

Usage:
    .venv/bin/python harness/validate_mugen.py
"""

from __future__ import annotations

import sys

import desbordante
import mugen
import polars as pl
import pyarrow.csv as pacsv

from desbordante_harness import DATASETS, REPO_ROOT

SUBSETS = ("cargo-subset14", "cargo-subset20", "cargo-subset30")


def desbordante_fds(path: str) -> set[tuple[frozenset[str], str]]:
    algo = desbordante.fd.algorithms.Pyro()
    algo.load_data(table=(path, ",", True))
    algo.execute(error=0.0, seed=0)
    return {(frozenset(lhs), rhs) for lhs, rhs in (fd.to_name_tuple() for fd in algo.get_fds())}


def mugen_fds(names: list[str], fds: list[tuple[list[int], int]]) -> set[tuple[frozenset[str], str]]:
    return {(frozenset(names[i] for i in lhs), names[rhs]) for lhs, rhs in fds}


def main() -> int:
    ok = True
    for name in SUBSETS:
        spec = DATASETS[name]
        path = str(REPO_ROOT / spec.path)
        want = desbordante_fds(path)

        names, fds = mugen.discover_fds(pacsv.read_csv(path))
        got_arrow = mugen_fds(names, fds)
        names, fds = mugen.discover_fds(pl.read_csv(path, separator=spec.separator))
        got_polars = mugen_fds(names, fds)

        match = got_arrow == want and got_polars == want
        ok &= match
        print(
            f"{name}: desbordante={len(want)} mugen_arrow={len(got_arrow)} "
            f"mugen_polars={len(got_polars)} match={match}"
        )
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
