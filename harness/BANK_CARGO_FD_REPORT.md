# Functional dependencies in the bank-marketing and Cargo datasets

This report describes the exact functional dependencies (FDs) that `mugen` finds in two UCI datasets. It covers counts by LHS-size cap, the structure of the results, and the caveats that affect how they should be read.

## Method

- Engine: `mugen.discover_fds(data, max_error=0.0, max_lhs=cap, threads=None)`. Exact FDs only (`max_error=0`). `threads=None` uses all cores.
- Caps: `max_lhs` = 1, 2, 3, and uncapped. A cap returns the minimal FDs whose LHS has at most that many columns, so each capped result is a subset of the uncapped one.
- Inputs:
  - **bank-full**: `bank+marketing/bank/bank-full.csv`, `;` separator, 45,211 rows × 17 columns.
  - **Cargo**: `cargo+2000+freight+tracking+and+tracing/c2k_data_comma.csv`, 3,943 rows × 98 columns (all columns, including the `nr` row counter).
- Timings are from the same single run per cap, with all threads. Treat the sub-second figures as indicative only.

## Results

### bank-full (45,211 × 17)

| Cap (max LHS) | FDs | Time |
|---|---|---|
| 1 | 0 | — |
| 2 | 0 | — |
| 3 | 2 | — |
| Uncapped | 626 | 0.59 s |

- Uncapped LHS sizes: size 3 (2 FDs), size 4 (8), size 5 (55), size 6 (170), size 7 (213), size 8 (106), size 9 (54), size 10 (17), size 11 (1).
- No column is unique per row, so no FD has a key on the left.
- The two cap-3 FDs are `{balance, duration, pdays} → previous` and `{balance, duration, pdays} → poutcome`. These are the smallest FDs in the dataset.
- The most frequent RHS columns are `poutcome` (132 FDs), `y` (110), and `pdays` (102).
- The most frequent LHS columns are `duration` (519), `balance` (508), and `age` (393).

### Cargo 2000 (3,943 × 98)

| Cap (max LHS) | FDs | Time | Notes |
|---|---|---|---|
| 1 | 426 | 0.25 s | 291 have an exact key on the LHS; 135 do not |
| 2 | 14,299 | 2.0 s | 426 size-1 + 13,873 size-2 |
| Uncapped | not run | — | Timed out in earlier runs for both engines |

- Cap 1 matches the count in [BENCHMARK_REPORT.md](./BENCHMARK_REPORT.md) (426). An earlier run that dropped `nr` gave 327. The difference is the `nr` column, which is itself a key.
- Columns that are unique per row: `nr`, `i1_legid`, `o_legid`. Any FD whose LHS is one of these holds trivially, and 291 of the 426 cap-1 FDs are of this kind.
- The 135 non-trivial cap-1 FDs are mostly driven by:
  - `i2_legid` as LHS (48 FDs), which is 67% unique per row, so it is close to a key.
  - `i3_legid` as LHS (23 FDs).
  - Columns of the `*_dep_3_*` and `*_rcf_3_*` families, such as `i1_dep_3_e` → `i1_dep_3_p`.
- **Mutual single-column FDs.** Of the 426 single-column FDs, 15 pairs determine each other. All 15 are one-to-one (the pair has the same number of distinct values as each column alone). Examples: `i1_dep_3_e` ↔ `i1_dep_3_p` and `i2_rcf_3_p` ↔ `i2_rcf_3_place`. This is consistent with these columns carrying the same information in different encodings. That interpretation is an inference from the data, not something checked against the dataset documentation.

## Caveats

1. **Trivial FDs from near-keys.** A column that is unique or nearly unique per row determines almost every other column. The "near-key" split in the working script used a 50% unique threshold. That is a rough cut. For Cargo, cap 2 gives 350 near-key FDs under this threshold. The bank counts have no near-keys, so the bank results need no such filter.
2. **Exact FDs only.** Real data has noise. These results do not allow for even one bad row, so an FD can fail because of a single typo.
3. **No significance testing.** On 45,211 rows, a large LHS (6+ columns) can determine a column by chance, because the number of LHS combinations is large. The bank results with big LHS sets should be read as observations, not as domain facts.
4. **Parity with Desbordante is not re-checked here.** The bank count (626) matches the existing benchmark report. The name-level FD set has not been compared with Desbordante in this report. The Cargo cap-1 count matches the benchmark report, and the Cargo parity status is described there.
5. **Cargo uncapped results are missing.** Both engines time out on the full uncapped run, so the report covers only caps 1 and 2 for Cargo.
6. **Instance count.** UCI lists 3,942 Cargo instances. The file has 3,943 rows, as noted in the [README](../README.md).
