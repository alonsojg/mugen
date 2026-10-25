//! Pyro-style discovery of minimal (approximate) functional dependencies.
//!
//! Like Desbordante's Pyro, every right-hand side gets its own search space,
//! and the spaces run in parallel. Within a space, the search follows Pyro's
//! launch-pad / ascend cycle:
//!
//! * Candidate left-hand sides are minimal hitting sets of the complements of
//!   the known non-dependencies (launch pads).
//! * A candidate that is a dependency is a minimal dependency. Monotonicity of
//!   the G1 error makes every subset of a non-dependency a non-dependency, so
//!   this minimality holds.
//! * A candidate that is not a dependency is ascended to a maximal
//!   non-dependency, which is recorded and updates the hitting sets.
//!
//! The search stops once every candidate is a dependency. At that point the
//! candidates are exactly the minimal dependencies.

use rayon::prelude::*;
use rustc_hash::FxHashMap;

use crate::column_set::ColumnSet;
use crate::discovery::Fd;
use crate::partition::{ProbeScratch, StrippedPartition, ValueId};

/// Minimal FDs `lhs -> rhs` whose G1 error is at most `max_error`.
///
/// With `max_error = 0.0` these are the exact minimal FDs, matching
/// [`discover_fds`](crate::discover_fds).
pub fn discover_fds_pyro(columns: &[StrippedPartition], max_error: f64) -> Vec<Fd> {
    let Some(first) = columns.first() else {
        return Vec::new();
    };
    let num_rows = first.relation_size();
    let column_tables: Vec<Vec<ValueId>> = columns.par_iter().map(|c| c.probing_table()).collect();
    (0..columns.len())
        .into_par_iter()
        .flat_map_iter(|rhs| {
            RhsSearch::new(columns, &column_tables, num_rows, rhs, max_error)
                .run()
                .into_iter()
                .map(move |lhs| Fd { lhs, rhs })
        })
        .collect()
}

struct RhsSearch<'a> {
    columns: &'a [StrippedPartition],
    /// Probing tables of `columns`, built once and shared across right-hand sides.
    column_tables: &'a [Vec<ValueId>],
    /// Buffers reused by every probe in this search.
    scratch: ProbeScratch,
    rhs: usize,
    universe: ColumnSet,
    /// Largest number of violating row pairs that still counts as a dependency.
    max_violations: f64,
    /// Partitions of visited left-hand sides, keyed by column set.
    cache: FxHashMap<ColumnSet, StrippedPartition>,
    /// Violating row pairs of `lhs -> rhs`, keyed by `lhs`.
    violations: FxHashMap<ColumnSet, u64>,
    /// Minimal hitting sets of the complements of the non-dependencies found so far.
    transversals: Vec<ColumnSet>,
}

impl<'a> RhsSearch<'a> {
    fn new(
        columns: &'a [StrippedPartition],
        column_tables: &'a [Vec<ValueId>],
        num_rows: u32,
        rhs: usize,
        max_error: f64,
    ) -> Self {
        let num_columns = columns.len();
        let n = f64::from(num_rows);
        let total_pairs = n * (n - 1.0) / 2.0;
        let mut cache = FxHashMap::default();
        cache.insert(
            ColumnSet::empty(num_columns),
            StrippedPartition::from_values(&vec![0; num_rows as usize]),
        );
        Self {
            columns,
            column_tables,
            scratch: ProbeScratch::default(),
            rhs,
            universe: ColumnSet::from_columns(num_columns, (0..num_columns).filter(|&c| c != rhs)),
            max_violations: max_error * total_pairs,
            cache,
            violations: FxHashMap::default(),
            transversals: vec![ColumnSet::empty(num_columns)],
        }
    }

    fn run(mut self) -> Vec<ColumnSet> {
        loop {
            let mut pending = Vec::new();
            for t in self.transversals.clone() {
                if !self.is_dependency(&t) {
                    pending.push(t);
                }
            }
            if pending.is_empty() {
                return self.transversals;
            }
            for t in pending {
                // An earlier ascent may have replaced `t` in the hitting sets.
                if !self.transversals.contains(&t) {
                    continue;
                }
                let maximal = self.maximize(t);
                self.add_non_dependency(maximal);
            }
        }
    }

    fn is_dependency(&mut self, lhs: &ColumnSet) -> bool {
        self.violations(lhs) as f64 <= self.max_violations
    }

    /// Violating row pairs of `lhs -> rhs`: pairs equal on `lhs` but not on `rhs`.
    fn violations(&mut self, lhs: &ColumnSet) -> u64 {
        if let Some(&v) = self.violations.get(lhs) {
            return v;
        }
        self.ensure_partition(lhs);
        let lhs_part = &self.cache[lhs];
        let joint = lhs_part.probe_with(&self.column_tables[self.rhs], &mut self.scratch);
        let v = lhs_part.nep() - joint.nep();
        self.violations.insert(lhs.clone(), v);
        v
    }

    fn ensure_partition(&mut self, lhs: &ColumnSet) {
        if self.cache.contains_key(lhs) {
            return;
        }
        let last = lhs.max().expect("the empty set is cached at construction");
        let rest = lhs.without(last);
        let part = if rest.is_empty() {
            self.columns[last].clone()
        } else {
            self.ensure_partition(&rest);
            self.cache[&rest].probe_with(&self.column_tables[last], &mut self.scratch)
        };
        self.cache.insert(lhs.clone(), part);
    }

    /// Grows a non-dependency to a maximal non-dependency inside the universe.
    ///
    /// A column rejected earlier stays rejected as the set grows, because
    /// supersets of a dependency are dependencies. One pass is therefore enough.
    fn maximize(&mut self, mut lhs: ColumnSet) -> ColumnSet {
        let candidates: Vec<usize> = self.universe.columns().collect();
        for c in candidates {
            if lhs.contains(c) {
                continue;
            }
            let extended = lhs.with(c);
            if !self.is_dependency(&extended) {
                lhs = extended;
            }
        }
        lhs
    }

    /// Updates the minimal hitting sets for a new non-dependency `non_dep`.
    ///
    /// This is Berge's incremental transversal step. A hitting set that already
    /// meets the new complement is kept. Otherwise it is extended by each
    /// element of the complement. The result is then minimized.
    fn add_non_dependency(&mut self, non_dep: ColumnSet) {
        let complement: Vec<usize> = self
            .universe
            .columns()
            .filter(|&c| !non_dep.contains(c))
            .collect();
        let mut candidates = Vec::new();
        for t in std::mem::take(&mut self.transversals) {
            if !t.is_subset_of(&non_dep) {
                candidates.push(t);
            } else {
                candidates.extend(complement.iter().map(|&c| t.with(c)));
            }
        }
        candidates.sort_by_key(ColumnSet::len);
        let mut minimal: Vec<ColumnSet> = Vec::with_capacity(candidates.len());
        for s in candidates {
            if !minimal.iter().any(|r| r.is_subset_of(&s)) {
                minimal.push(s);
            }
        }
        self.transversals = minimal;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::discover_fds;

    fn normalize(fds: &[Fd]) -> Vec<(Vec<usize>, usize)> {
        let mut v: Vec<(Vec<usize>, usize)> = fds
            .iter()
            .map(|fd| (fd.lhs.columns().collect(), fd.rhs))
            .collect();
        v.sort();
        v
    }

    fn partitions(rows: &[Vec<u32>], num_columns: usize) -> Vec<StrippedPartition> {
        (0..num_columns)
            .map(|c| {
                let values: Vec<u32> = rows.iter().map(|r| r[c]).collect();
                StrippedPartition::from_values(&values)
            })
            .collect()
    }

    /// Brute-force minimal approximate FDs, enumerating every left-hand side.
    fn brute_force_minimal(cols: &[StrippedPartition], max_error: f64) -> Vec<(Vec<usize>, usize)> {
        let n = cols.len();
        let num_rows = cols[0].relation_size();
        let pairs = f64::from(num_rows) * (f64::from(num_rows) - 1.0) / 2.0;
        let lhs_partition = |mask: usize| {
            let mut part = StrippedPartition::from_values(&vec![0; num_rows as usize]);
            for c in (0..n).filter(|c| mask & (1 << c) != 0) {
                part = part.intersect(&cols[c]);
            }
            part
        };
        let dep = |mask: usize, rhs: usize| {
            let lhs = lhs_partition(mask);
            let joint = lhs.intersect(&cols[rhs]);
            (lhs.nep() - joint.nep()) as f64 <= max_error * pairs
        };
        let mut out = Vec::new();
        for rhs in 0..n {
            for mask in 0..(1usize << n) {
                if mask & (1 << rhs) != 0 || !dep(mask, rhs) {
                    continue;
                }
                let minimal = (0..n)
                    .filter(|c| mask & (1 << c) != 0)
                    .all(|c| !dep(mask & !(1 << c), rhs));
                if minimal {
                    let lhs: Vec<usize> = (0..n).filter(|c| mask & (1 << c) != 0).collect();
                    out.push((lhs, rhs));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn exact_matches_level_wise_discovery_on_random_tables() {
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move |bound: u32| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) as u32) % bound
        };

        for _ in 0..150 {
            let num_columns = 2 + next(4) as usize;
            let num_rows = 1 + next(12) as usize;
            let rows: Vec<Vec<u32>> = (0..num_rows)
                .map(|_| (0..num_columns).map(|_| next(3)).collect())
                .collect();
            let cols = partitions(&rows, num_columns);

            assert_eq!(
                normalize(&discover_fds_pyro(&cols, 0.0)),
                normalize(&discover_fds(&cols)),
                "rows: {rows:?}"
            );
        }
    }

    #[test]
    fn approximate_matches_brute_force_on_random_tables() {
        let mut state: u64 = 0x1B87_3593_9E37_79B9;
        let mut next = move |bound: u32| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) as u32) % bound
        };

        for _ in 0..60 {
            let num_columns = 2 + next(4) as usize;
            let num_rows = 2 + next(14) as usize;
            let rows: Vec<Vec<u32>> = (0..num_rows)
                .map(|_| (0..num_columns).map(|_| next(4)).collect())
                .collect();
            let cols = partitions(&rows, num_columns);

            for max_error in [0.0, 0.1, 0.25, 0.5] {
                assert_eq!(
                    normalize(&discover_fds_pyro(&cols, max_error)),
                    brute_force_minimal(&cols, max_error),
                    "max_error {max_error}, rows: {rows:?}"
                );
            }
        }
    }

    #[test]
    fn constant_column_yields_empty_lhs_fd() {
        let rows = vec![vec![7, 1], vec![7, 2], vec![7, 3]];
        let cols = partitions(&rows, 2);
        assert_eq!(
            normalize(&discover_fds_pyro(&cols, 0.0)),
            vec![(Vec::new(), 0)]
        );
    }
}
