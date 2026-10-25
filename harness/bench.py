"""Times Desbordante's FD algorithms on the bank datasets for comparison with
the Rust TANE port.

Each run loads the CSV and discovers FDs, matching how the harness calls
Desbordante. The best of ``--rounds`` runs is reported. Single-threaded.
"""

from __future__ import annotations

import argparse
import time
from pathlib import Path

import desbordante

from desbordante_harness import DATASETS, REPO_ROOT

ALGORITHMS = {
    "Pyro": (desbordante.fd.algorithms.Pyro, {"error": 0.0, "seed": 0}),
    "DFD": (desbordante.fd.algorithms.DFD, {}),
}


def time_algorithm(cls, options: dict, path: Path, separator: str, rounds: int) -> tuple[float, int]:
    best = float("inf")
    count = 0
    for _ in range(rounds):
        algo = cls()
        start = time.perf_counter()
        algo.load_data(table=(str(path), separator, True))
        algo.execute(threads=1, **options)
        best = min(best, time.perf_counter() - start)
        count = len(algo.get_fds())
    return best, count


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rounds", type=int, default=3)
    args = parser.parse_args()

    for dataset in DATASETS.values():
        path = REPO_ROOT / dataset.path
        for name, (cls, options) in ALGORITHMS.items():
            seconds, count = time_algorithm(cls, options, path, dataset.separator, args.rounds)
            print(f"{dataset.name}\t{name}\tfds={count}\ttotal={seconds:.4f}")


if __name__ == "__main__":
    main()
