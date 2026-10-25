# mugen

Rust-based data profiling for functional dependency (FD) discovery, with a
Python module built on Polars and Arrow. The goal is to take the search
work that [Desbordante](https://github.com/Mstrutov/Desbordante) does in C++
and run it in Rust, so that a Polars pipeline can discover FDs faster than
pandas plus Desbordante.

## Why this project exists

Desbordante is a C++ library for data profiling. It discovers FDs, unique
column combinations (UCCs), and inclusion dependencies (INDs), and it ships
with Python bindings. Its algorithms are strong, but its Python path has
overhead. A DataFrame has to be copied into Desbordante's own table format,
and the result has to be copied back into Python objects.

Two things drive the design here:

1. **Throughput.** FD discovery depends on two primitives: stripped partitions
   (equivalence classes of row ids that share values) and candidate lattices
   (sets of column combinations stored as bitsets). Both can be written in
   Rust with flat `Vec` storage and index handles, without raw pointers or
   `Rc<RefCell<_>>`.
2. **Zero-copy data flow.** Rust should read Arrow buffers directly through
   the Arrow PyCapsule stream protocol. Polars and pyarrow both speak that
   protocol, so the same entry point works for either.

## Where it comes from

The Rust code is a port of Desbordante's algorithms, not a wrapper around
it. Desbordante is used in three places:

- **Ground truth.** `harness/desbordante_harness.py` runs Desbordante and
  pandas-based checks, and writes the exact FDs, UCCs, and INDs to JSON
  fixtures in `harness/fixtures/`.
- **Reference timings.** `harness/bench.py` times Desbordante with one thread
  so the Rust results can be compared against it.
- **Pipeline comparison.** `harness/pipeline_bench.py` times the whole path
  (data in memory to discovered FDs) for pandas plus Desbordante and for
  mugen.

Ported so far:

| Component | Location | Status |
|---|---|---|
| Stripped partitions (CSR `rows` + `offsets`) | `rust/stripped_partition/src/partition.rs` | Done |
| Column sets and lattice | `column_set.rs`, `lattice.rs` | Done |
| TANE (exact, brute-force reference) | `tane.rs` | Done, not exposed to Python |
| Approximate measures | `afd.rs` | Done, not exposed to Python |
| Pyro (per-RHS search, LRU partition cache) | `pyro.rs` | Done, exposed as `mugen.discover_fds` |
| PyO3 module on Arrow C stream | `rust/mugen/src/lib.rs` | Done |

## How it improves on Desbordante

- **Parallel search across right-hand sides.** Pyro runs one RHS per task on
  a rayon pool. `threads=` sets a dedicated pool, and the default uses all
  cores. The result does not depend on the thread count (there is a unit test
  for that).
- **Budgeted partition cache.** The partition cache is bounded by an LRU
  budget (2 GiB by default, split across threads), so memory does not grow
  without limit as the search visits more left-hand sides.
- **Arrow-native input.** Any object with `__arrow_c_stream__` works, including
  `pyarrow.Table` and Polars `DataFrame`. Columns are dictionary-encoded into
  `u32` codes. Integer columns are read directly from the Arrow buffer.
- **Exact validation.** Every FD set is compared by name against Desbordante's
  output (`harness/validate_mugen.py`). The sets match on the 14, 20, and
  30 column subsets of the Cargo dataset, through both pyarrow and Polars.

## Results so far

Numbers below come from the reports in `harness/`. Each one is from one
machine (Ryzen 9 5900X) on one dataset, and the caveats in those reports apply.

| Measurement | Desbordante | mugen / Rust | Notes |
|---|---:|---:|---|
| Cargo 14 cols, 1 thread | 0.071 s | 0.030 s (2.4×) | 414 FDs, match |
| Cargo 20 cols, 1 thread | 0.219 s | 0.088 s (2.5×) | 946 FDs, match |
| Cargo 30 cols, 1 thread | 2.42 s | 0.76 s (3.2×) | 8,250 FDs, match |
| Cargo 30 cols, all threads | n/a | 0.12 s (20×) | same FDs |
| Cargo 98 cols, uncapped | timeout (20 min) | timeout (20 min) | neither finishes |
| Cargo 98 cols, LHS cap 1 | timeout (15 min) | 0.25 s | 426 FDs |

Memory is higher with threads: at 30 columns, peak RSS was 382 MB with all
threads and 106 MB with one thread, against 182 MB for Desbordante.

## Conclusion

### Benefits

- Pyro is faster than Desbordante's Pyro at every column count tested, and the
  gap grows with the width of the table.
- DataFrame input is read through Arrow, so there is no per-row conversion
  into Python objects. Desbordante needs that conversion.
- The search is parallel across right-hand sides and has a memory budget.
- The Rust code is covered by unit tests (`cargo test --release`), and its
  output is checked against Desbordante on real data.

### Drawbacks and open issues

- **Only Pyro is exposed.** TANE and the approximate measures are ported but
  not reachable from Python. The approximate path (`max_error > 0`) has no
  parity check yet.
- **The 98-column dataset is still out of reach.** Neither engine finishes it
  uncapped in 20 minutes. Desbordante does not finish even with a cap of 1.
- **The partition cache is bounded, but the output is not.** The list of
  minimal FDs grows with the search. At cap 3 on the full Cargo data, peak RSS
  was 2.67 GB.
- **Null semantics are checked only on a toy table.** Nulls are treated as
  equal to each other, and Desbordante agreed on a 5-row example. That is not
  proof for every null pattern.
- **Arrow ingest copies data.** The CSR partitions are built from dictionary
  codes, so the path is not yet zero-copy. Encoding is only about 4% of a
  full-file run, so the copy is not the bottleneck today.
- **Timings are single runs or best-of-3 on one machine,** with other load
  present. Differences under about 100 ms should be treated as noise.
- **Desbordante's HyFD ground truth disagreed with the pandas check** on 9
  FDs. HyFD is therefore excluded from the ground truth. The cause is
  suspected but not confirmed.

## Future plans

### Algorithms to add and test

1. **Expose TANE and approximate Pyro from Python**, with the same name-based
   parity check against Desbordante, including `max_error > 0`.
2. **Unique column combinations (UCCs)** using the same lattice, and a
   parity check against Desbordante's UCC output.
3. **Inclusion dependencies (INDs)**, which need a different structure
   (value sets per column) from the FD path. SPIDER-style sorted merging is
   the first candidate.
4. **HyFD and FastFDs**, if the ground-truth question is resolved, so that the
   sampling-based path can be compared with exact search on the same data.
5. **Other Desbordante FD algorithms** from the benchmark list, for speed
   comparisons only.

### Architecture ideas

- **Zero-copy CSR ingest.** Build partitions directly from Arrow arrays, with
  no intermediate code vector per column.
- **Streaming and batching.** Process record batches as they arrive, so a
  table larger than memory can be profiled in chunks.
- **Bounded output.** Stream minimal FDs to a callback or iterator instead of
  holding them all in memory, and add a limit on the number of results.
- **Polars plugin.** Expose discovery as a Polars expression or plugin, so the
  call fits in a lazy query.
- **Configurable budgets.** Let callers set the cache budget and thread count
  per call, and report the actual peak memory.
- **Continuous checks.** Run the Desbordante parity scripts in CI on the
  subsets and fixtures, so a regression in any algorithm is caught.

## Repository layout

```
harness/           Python harness: Desbordante ground truth, benchmarks, reports
  desbordante_harness.py   runs Desbordante, writes JSON fixtures
  bench.py                 Desbordante timing (one thread)
  pipeline_bench.py        end-to-end timing, one subprocess per side
  validate_mugen.py        FD parity check against Desbordante
  mugen_walkthrough.ipynb  walkthrough of mugen versus Desbordante
  BENCHMARK_REPORT.md      Rust versus Desbordante, runtime and memory
  PIPELINE_REPORT.md       pandas plus Desbordante versus polars plus mugen
rust/stripped_partition/   core Rust library (partitions, lattice, TANE, Pyro)
rust/mugen/                PyO3 module exposed to Python as `mugen`
```

## Setup and reproduction

```bash
python -m venv .venv
.venv/bin/pip install -r harness/requirements.txt
.venv/bin/pip install maturin pyarrow polars
.venv/bin/maturin develop --release -m rust/mugen/Cargo.toml

# Rust unit tests
cd rust/stripped_partition && cargo test --release
cd ../mugen && cargo test --release

# FD parity against Desbordante
cd /path/to/mugen && .venv/bin/python harness/validate_mugen.py
```

The UCI datasets are not in git. Put the bank marketing and Cargo 2000
archives in the repository root and extract them there. The Cargo subsets
(`c2k_subset14/20/30.csv`) are the first 14, 20, and 30 data columns of
`c2k_data_comma.csv`, with the `nr` row counter excluded. No script in the
repository creates them, so they must be cut by hand.
