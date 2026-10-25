# Benchmark Report: Rust `stripped_partition` vs Desbordante

This report compares the Rust port (`rust/stripped_partition`) against Desbordante 2.5.0 on the UCI Bank Marketing datasets. It covers runtime, peak memory, and the Rust optimization history. Read the [Caveats](#caveats) section before drawing conclusions.

## Setup

| Item | Value |
|---|---|
| CPU | AMD Ryzen 9 5900X, 12 cores / 24 threads |
| RAM | 125 GB |
| Rust build | `cargo build --release` / `cargo test --release` |
| Desbordante | 2.5.0 (Python), venv at `.venv/` |
| Datasets | UCI Bank Marketing: `bank.csv`, `bank-full.csv`, `bank-additional.csv`, `bank-additional-full.csv`; Cargo 2000 Freight Tracking and Tracing: `c2k_data_comma.csv` (98 columns) and `c2k_subset14.csv` (14 columns, see section 3a) |
| Dataset size | `bank.csv` 4,521 data rows; `bank-full.csv` 45,211 data rows |
| Ground truth | Minimal exact FDs from Desbordante Pyro, DFD, and HyFD (see [caveats](#caveats)) |

Exact minimal FD counts (ground truth, verified by the Rust port):

| Dataset | FDs |
|---|---|
| bank | 1,342 |
| bank-full | 626 |
| bank-additional | 3,584 |
| bank-additional-full | 1,509 |

The Rust Pyro and TANE outputs match these counts exactly. The fixture tests check this on every run.

## 1. Runtime: Pyro (FD discovery)

### 1a. Single-threaded, current Rust vs Desbordante

Both sides use one thread.

| Dataset | Desbordante Pyro (s) | Rust Pyro, current (s) | Rust speedup |
|---|---:|---:|---:|
| bank | 0.84 | 0.38 | 2.2× |
| bank-full | 2.77 | 2.21 | 1.25× |
| bank-additional | 3.11 | 1.03 | 3.0× |
| bank-additional-full | 10.27 | 5.97 | 1.7× |

Desbordante times come from `harness/fixtures/_timings.json` (`threads = 1`; the number of runs per value is not recorded in the file). Rust times come from `fd_bench` with `RAYON_NUM_THREADS=1`, measured after commit `1221e2b`. The Rust timings are the `discover` phase only. Dataset loading is not included.

### 1b. Rust Pyro progression (single-threaded)

| Dataset | Initial port (s) | + precomputed probing tables (s) | + scratch reuse and counting-only violations (s) |
|---|---:|---:|---:|
| bank | 0.51 | 0.45 | 0.38 |
| bank-full | 2.98 | 2.60 | 2.21 |
| bank-additional | 1.35 | 1.21 | 1.03 |
| bank-additional-full | 7.76 | 7.12 | 5.97 |

Overall single-threaded gain since the first working port: about 25% on `bank`, 26% on `bank-full`, 24% on `bank-additional`, and 23% on `bank-additional-full`.

### 1c. Rust Pyro, all threads (earlier measurement)

These numbers are from the first Pyro port, before the single-thread optimizations. They were not re-measured after them.

| Dataset | Rust Pyro, all threads (s) |
|---|---:|
| bank | 0.09 |
| bank-full | 0.68 |
| bank-additional | 0.23 |
| bank-additional-full | 1.51 |

For reference, Desbordante Pyro single-threaded (best of 3 in the first run) was 0.82 / 2.68 / 2.99 / 9.60 s on the same four datasets, in that order.

### 1d. Desbordante's other FD and UCC algorithms (for context)

Single-threaded, from `_timings.json`. There is no Rust counterpart for these.

| Dataset | DFD (s) | Pyro FD (s) | HyUCC (s) | PyroUCC (s) | Spider IND (s) |
|---|---:|---:|---:|---:|---:|
| bank | 1.12 | 0.84 | 0.048 | 0.104 | 0.007 |
| bank-full | 4.90 | 2.77 | 0.236 | 0.307 | 0.062 |
| bank-additional | 5.28 | 3.11 | 0.105 | 0.241 | 0.007 |
| bank-additional-full | 15.27 | 10.27 | 0.333 | 0.238 | 0.066 |

## 2. Runtime: TANE vs `discover_fds` (brute-force reference)

These measurements were taken during the TANE port, before the Pyro work. The brute-force `discover_fds` is the reference implementation in this crate, not Desbordante.

| Algorithm | Time (s) | Notes |
|---|---:|---|
| `discover_fds` (brute force) | 12.88 | Recorded as "bank" in session notes; see caveats |
| `discover_fds_tane` (all threads, before rayon/cache work) | 1.45 | Recorded as "bank"; matches the later bank-full all-thread figure |

Later TANE timings with all threads, after the rayon and probing-table work:

| Dataset | TANE time | Notes |
|---|---:|---|
| bank-full | 1.45 s | Same figure as above; the dataset label is inconsistent in the session notes |
| bank-additional-full | ~199 s before the memory change, ~213 s after | Difference is likely noise; not re-measured |

Rust TANE was not timed against Desbordante on all four datasets, so no direct TANE-vs-Desbordante ratio is reported.

## 3. Memory: peak RSS (single-threaded)

Peak RSS is measured with `/usr/bin/time -f %M`. Both sides run one thread (`RAYON_NUM_THREADS=1` for Rust, `threads=1` for Desbordante).

| Dataset | Rust Pyro (MB) | Desbordante Pyro (MB) | Rust TANE, before memory fix (MB) | Rust TANE, after memory fix (MB) |
|---|---:|---:|---:|---:|
| bank | 20 | 67 | 200 | 120 |
| bank-full | 161 | 209 | 2,780 | 1,540 |
| bank-additional | 50 | 133 | 1,820 | 1,060 |
| bank-additional-full | 419 | 802 | 29,200 | 15,900 |

Rust Pyro uses less memory than Desbordante Pyro on all four datasets, single-threaded. Rust TANE is the memory-heavy algorithm: on `bank-additional-full` it peaked at about 15.9 GB even after the fix. Pyro is the better choice for the larger datasets.

Memory for the all-thread Rust runs is not comparable (see caveats). In the first all-thread pass the Rust peaks were 112 / 650 / 273 / 2,030 MB for Pyro, and 211 MB / 2.87 GB / 1.89 GB / 29.2 GB for TANE. Those numbers are inflated by several datasets being loaded at once, so they are not reported as results.

Desbordante DFD peak memory was 116 / 343 / 308 / 2,130 MB in the first all-thread pass. It was not re-measured single-threaded.

## 3a. Cargo 2000 Freight Tracking and Tracing (98 columns)

The full dataset has 98 columns and 3,943 rows. The subset has 14 columns (original columns 2–10, 23–25, 74, and 98) and the same 3,943 rows. The subset exists so that both implementations can finish on a search space they can both handle.

Full dataset, timings are single runs. Desbordante ran with `threads=1`. Rust ran on all 24 threads, because the default `fd_bench` configuration does not pin threads:

| Implementation | LHS cap | FDs found | Time (s) | Peak RSS | Status |
|---|---|---:|---:|---:|---|
| Rust Pyro | 2 | 14,299 | 2.2 | not measured | finished |
| Rust Pyro | 3 | 258,718 | 107–124 (3 runs) | 2.67 GB | finished |
| Rust Pyro | none | — | >3,600 | ~3.1 GB at timeout | timed out (exit 124) |
| Desbordante Pyro | 2 | — | >1,800 | ~220 MB at 28 min | timed out (exit 124) |
| Desbordante Pyro | 3 | — | >1,800 | ~200 MB at 21 min | timed out (exit 124) |

Desbordante returned no result on the full dataset at either cap within 30 minutes. A speedup on this dataset cannot be reported. Rust returned results at both caps, and the uncapped Rust run did not finish within 60 minutes.

Subset, 14 columns, single run each. Timing includes dataset loading on both sides:

| Implementation | LHS cap | FDs | Total time (s) |
|---|---|---:|---:|
| Rust Pyro | 3 | 255 | 0.095 |
| Desbordante Pyro | 3 | 255 | 0.184 |
| Rust Pyro | none | 414 | 0.121 |
| Desbordante Pyro | none | 414 | 0.204 |

FD counts agree exactly at both caps. Rust is about 2× faster on this subset in these single runs. The noise level was not measured, so the ratio is indicative only.

## 4. What changed in the Rust code

| Change | Effect |
|---|---|
| Stripped partitions as flat CSR `Vec<u32>` | Basis for everything else |
| TANE with `FxHashMap` | Hashing overhead reduced |
| Probing-table cache (TANE) | Single-thread TANE faster; no change in output |
| Rayon over parents in `generate_next` (TANE) | Parallel speedup on multi-core |
| Free parent partitions once children are built (TANE) | Memory roughly halved on large datasets |
| Per-RHS Pyro search, parallel over RHS | Main parallel speedup for Pyro |
| Precomputed probing table per column (Pyro) | About 10–13% faster single-threaded |
| Scratch buffers reused across probes (Pyro) | Small gain, about 3% |
| Counting-only violation check (Pyro) | About 8–16% faster; no partition built for the check |
| Per-thread LRU budget for cached partitions (Pyro) | Caps partition memory at 2 GiB total by default; no change in output |
| LHS-size cap (Pyro, `max_lhs`) | Lets the search finish on wide tables; reports minimal FDs up to the cap only |
| Violation memo bounded by the budget (Pyro) | Memo is cleared when full and recomputed on demand; no change in output |

A 1-thread TANE fix for memory changed the peak but not the discovered FDs.

## Caveats

1. **Desbordante Pyro is approximate, Rust is exact.** Desbordante's Pyro uses sampling and confidence intervals to estimate error. The Rust port computes exact G1 for every candidate. The two can differ in work done and in accuracy. Results are not directly equivalent.

2. **Thread counts differ across measurements.** Desbordante numbers are single-threaded. The all-thread Rust Pyro numbers (section 1c) are not comparable with single-thread Desbordante numbers. The like-for-like comparison is section 1a.

3. **Single-threaded Rust timings are from one run per dataset, not best-of-N.** Run-to-run variation is not quantified. Run-to-run variation was not measured for either side. The first Desbordante Pyro run (best of 3) differs from the `_timings.json` value by 2–7% across datasets, which gives a rough sense of the noise.

4. **Timing scope.** Rust times are the `discover` phase only. Desbordante times in `_timings.json` cover the algorithm call. Input loading and Python overhead are not included on either side. Desbordante's memory includes the Python interpreter, while Rust memory is the bare binary. This makes the Rust memory advantage slightly larger than a same-process comparison would show.

5. **Peak RSS is process-level.** It includes allocator behavior and any memory that was not returned to the OS.

6. **First-pass memory numbers were inflated.** The first all-thread run used substring dataset filters (`bank`, `bank-additional`) that matched several datasets, so some processes loaded more than one. Only the anchored-filter, single-thread runs (section 3) are reported as results.

7. **HyFD was dropped from the ground truth.** HyFD reported 9 FDs that pandas shows are violated, so it was excluded. The likely cause is that HyFD's sampling-based validation can stop early when its difference sets are incomplete. This was not confirmed by inspecting HyFD's internals. The harness uses exact validation, so ground truth comes from Pyro and DFD agreement, not from HyFD.

11. **Pyro memory is bounded only partly.** The partition cache is capped by an LRU budget (2 GiB total by default, split per thread) and the violation memo is bounded by the same budget. The transversal list is the search output and is not bounded. Full 98-column runs without a cap grow past 3 GB, and the transversal list is the likely cause. The 2.67 GB peak at cap 3 on the Cargo dataset is consistent with a 2 GiB partition cache plus the transversal list. This split was not measured directly.


8. **`discover_fds` is a reference implementation in this crate, not Desbordante.** It is brute force and is used only to check correctness and time TANE.

9. **The "bank" label in the TANE timings is inconsistent.** The 12.88 s and 1.45 s figures were recorded as "bank", but 1.45 s also matches the later bank-full all-thread figure. The dataset for those numbers should be confirmed before publishing.

10. **TANE on the larger datasets is memory-bound.** Its peak on `bank-additional-full` is about 15.9 GB single-threaded, which is far above Pyro's 419 MB. Timing comparisons on that dataset should be read with this in mind.

11. **Pyro memory is bounded only partly.** The partition cache is capped by an LRU budget (2 GiB total by default, split per thread) and the violation memo is bounded by the same budget. The transversal list is the search output and is not bounded. Full 98-column runs without a cap grow past 3 GB, and the transversal list is the likely cause. The 2.67 GB peak at cap 3 on the Cargo dataset is consistent with a 2 GiB partition cache plus the transversal list. This split was not measured directly.


12. **Only FDs were benchmarked against Desbordante on the Rust side.** UCC and IND results from Desbordante are reported in section 1d for context, but there is no Rust implementation of them.

13. **Machine-specific.** All results come from one 12-core Ryzen 9 5900X machine (24 threads) with 125 GB RAM. Other hardware will give different absolute numbers.

14. **Profiling was limited.** `perf` was blocked by `perf_event_paranoid=4`, so the hot-spot analysis used temporary `Instant` timers and probe counters. Those counters were reverted and are not in the committed code.

15. **The 98-column results are single runs with timeouts.** Desbordante produced no result on the Cargo dataset at cap 2 or cap 3 within 30 minutes. The Rust cap-3 timings (107–124 s over three runs) have some spread. The uncapped Rust run did not finish within 60 minutes. No speedup claim is made for the full 98-column dataset.

16. **Cap semantics are verified only by counts.** Rust's cap was checked against Desbordante's `max_lhs` only by matching FD counts on the 14-column subset (255 at cap 3). On the full dataset, Rust's cap-2 and cap-3 counts have no Desbordante counterpart. Rust's cap is also tested against the uncapped output in a unit test.

## Reproducing

- Desbordante: `.venv/bin/python harness/bench.py --rounds 1 --algorithm Pyro --dataset <path>`
- Rust: `cd rust/stripped_partition && RAYON_NUM_THREADS=1 cargo run --release --example fd_bench -- /home/alonso/mugen 1 pyro <dataset-filter>`. Use anchored filters such as `/bank-full.csv`.
- Rust, Cargo cap 3: `cd rust/stripped_partition && cargo run --release --example fd_bench -- /home/alonso/mugen 1 pyro:3 c2k_data_comma`. Use `pyro` without a cap for the uncapped run. Use `c2k_subset14` for the 14-column subset.
- Desbordante, Cargo cap 3: `.venv/bin/python harness/bench.py --rounds 1 --algorithm Pyro --dataset c2k_data_comma --max-lhs 3`
- Correctness: `cargo test --release` (21 unit tests, 7 fixture tests).
