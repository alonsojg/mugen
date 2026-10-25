# Pipeline comparison: pandas + Desbordante vs polars + mugen

This report times the end-to-end path the project is aiming at: an in-memory
DataFrame goes in and the minimal FDs come out. The baseline is pandas with
Desbordante's Pyro. The candidate is polars with `mugen`, a Rust extension
built on `stripped_partition` that takes Arrow data through PyO3.

Earlier reports timed Rust against Desbordante on CSV files. This one covers
the handoff from the DataFrame into each engine, which is the cost a user of
either pipeline actually pays.

## Setup

- Data: Cargo 2000 freight tracking dataset, the 14, 20, and 30 column prefixes
  (`cargo-subset14`, `cargo-subset20`, `cargo-subset30`) and the full 98-column
  file (`cargo`). Each file has 3,943 rows and no real nulls. The only empty
  fields are from a trailing blank line.
- Machine: Ryzen 9 5900X, 12 cores / 24 threads, 125 GB RAM.
- Desbordante side: `pandas.DataFrame` built once outside the timer, then
  `load_data(table=df)` and `execute(error=0.0, seed=0, threads=1)`. Desbordante's
  conversion from pandas is inside the timer.
- mugen side: `polars.DataFrame` built once outside the timer, then
  `mugen.discover_fds(df, threads=1)` (mugen-1t) or `threads=None`, which uses
  all 24 threads (mugen-all). Arrow stream handoff, UTF-8 cast, dictionary
  encoding, and discovery are all inside the timer.
- Each side runs in its own process (`harness/pipeline_bench.py`), so peak RSS is
  per side and a timeout kills only that side.
- Timing: best of 3 rounds on the subsets. The full dataset ran a single round
  because of the time limit.

Reproduce with:

```
.venv/bin/python harness/validate_mugen.py
.venv/bin/python harness/pipeline_bench.py --rounds 3 --timeout 900 \
    --datasets cargo-subset14 cargo-subset20 cargo-subset30 --out /tmp/pipeline_subsets.json
.venv/bin/python harness/pipeline_bench.py --rounds 1 --timeout 1200 \
    --datasets cargo --sides desbordante mugen-all --out /tmp/pipeline_full.json
```

## Correctness

`harness/validate_mugen.py` compares the FD sets by column name, not by index.
Each mugen run uses both a pyarrow Table and a Polars DataFrame as input.

| Dataset | Desbordante FDs | mugen FDs (pyarrow) | mugen FDs (Polars) | Sets identical |
|---|---|---|---|---|
| subset14 | 414 | 414 | 414 | yes |
| subset20 | 946 | 946 | 946 | yes |
| subset30 | 8,250 | 8,250 | 8,250 | yes |

The full 98-column dataset is not in the validation: neither side finishes it
within the time limit, so there is no reference set to compare against.

## Results

Best of 3 rounds for the subsets. Speedups are relative to Desbordante on the
same subset.

| Dataset | Desbordante, 1 thread | mugen, 1 thread | mugen, 24 threads | mugen-1t speedup | mugen-all speedup |
|---|---|---|---|---|---|
| subset14 | 0.0709 s | 0.0298 s | 0.0081 s | 2.4× | 8.7× |
| subset20 | 0.2189 s | 0.0877 s | 0.0186 s | 2.5× | 11.8× |
| subset30 | 2.4172 s | 0.7616 s | 0.1174 s | 3.2× | 20.6× |
| cargo (98 cols) | timeout (20 min) | not run this round | timeout (20 min) | n/a | n/a |

Peak RSS per process, in MB. The value includes the Python interpreter and
imports, so it is a floor on what each side needs.

| Dataset | Desbordante, 1 thread | mugen, 1 thread | mugen, 24 threads |
|---|---|---|---|
| subset14 | 114 | 82 | 94 |
| subset20 | 123 | 86 | 147 |
| subset30 | 182 | 106 | 382 |

The 98-column run timed out for both sides at 20 minutes. The single-thread
mugen run was also a timeout in the earlier Pyro session, where it ran past
60 minutes. Each timeout is reported as a timeout, not a time.

## What the numbers say

- **Single core.** On the same algorithm and same thread count, mugen is 2.4–3.2×
  faster than Desbordante Pyro over DataFrames. The gap grows with width, as
  it did in the CSV comparison.
- **With all cores.** mugen-all gives 9–21× over single-thread Desbordante. Most
  of that is the parallel right-hand-side search in `discover_fds_pyro_limited`,
  not the Arrow layer. This matches the earlier Pyro port result.
- **Ingest cost.** The single-thread mugen total (0.762 s) is almost the same as
  the single-thread Rust discovery time measured separately with `fd_bench` and
  `RAYON_NUM_THREADS=1` (0.770 s). So the Arrow handoff and dictionary encoding
  add very little on these inputs. Wide string columns may cost more, since every
  string is hashed. That was not measured separately.
- **Memory.** mugen-all uses more RSS on subset30 (382 MB vs 182 MB). Each rayon
  worker has its own partition cache, with the 2 GiB total budget split between
  them. Desbordante's single-thread run is smaller. The cache is only filled as far
  as the search needs it, so this is an upper bound for this dataset, not a limit.
- **98 columns.** Neither engine finishes the uncapped search in 20 minutes. The
  Arrow layer does not change that. The bottleneck is the search itself, which
  the earlier Pyro reports already describe.

## Caveats

1. **Threads are not equal across the comparison.** Desbordante is fixed at one
   thread, which matches the earlier baselines. Desbordante's default is all
   cores. In an earlier validation run, which used the default thread count and
   CSV input, Desbordante Pyro took 0.771 s on subset30, versus 2.42 s here at one
   thread. Any claim about speed against Desbordante needs to name the thread
   count.
2. **Input type differs.** Desbordante gets a pandas frame and mugen gets a Polars
   frame, which is the comparison you asked for. Each side converts its own
   frame internally, and each conversion is inside its timer.
3. **Null handling.** mugen treats all nulls in a column as one value that equals
   each other and differs from any non-null value. The Cargo data has no nulls,
   so this path is not exercised by the main results. On a 5-row toy table with
   nulls (`harness/mugen_walkthrough.ipynb`, Step 10), Desbordante returned the
   same four FDs as mugen, so it also treats nulls as equal. This is one small
   case, not a proof for all null patterns.
4. **Dictionary encoding copies.** The Arrow stream is read without copying, but
   each column is cast to UTF-8 and each value is looked up in a hash map to get a
   `u32` code. That is a copy into the codes array, not the zero-copy CSR
   construction the planned milestone describes. Integer and float columns pay
   for a string round-trip.
5. **Unsafe code at the FFI boundary.** The only `unsafe` in `rust/mugen/src/lib.rs`
   reads the Arrow C stream pointer out of the PyCapsule. The stream protocol
   requires this. It moves the stream out of the capsule, so the producer cannot
   release it twice.
6. **Single runs for the full dataset.** The 98-column result is a single
   20-minute timeout per side. Repeating it would take another 40 minutes, and
   both sides already timed out in earlier runs.
7. **Rust tests.** `rust/mugen` has no unit tests. Correctness rests on
   `validate_mugen.py` against Desbordante, plus the existing `stripped_partition`
   tests, which cover the discovery code that mugen calls.
8. **Single machine, single run per point.** The subset numbers are best-of-3 on
   one machine with other load present, so they are noisy at the sub-100 ms
   level. Treat the 2.4–3.2× single-core figures as approximate.
