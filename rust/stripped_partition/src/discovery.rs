use rustc_hash::FxHashMap;

use crate::column_set::ColumnSet;
use crate::lattice::next_level;
use crate::partition::StrippedPartition;

/// Minimal unique column combinations, found level by level.
///
/// `columns[i]` is the stripped partition of the single column `i`. A set is
/// unique when its partition has no non-singleton cluster. Only non-unique
/// sets are expanded, so every reported UCC is minimal by construction.
pub fn discover_uccs(columns: &[StrippedPartition]) -> Vec<ColumnSet> {
    let n = columns.len();
    let mut uccs = Vec::new();
    let mut level: FxHashMap<ColumnSet, StrippedPartition> = FxHashMap::default();

    for (c, partition) in columns.iter().enumerate() {
        let set = ColumnSet::singleton(n, c);
        if partition.num_non_singleton_clusters() == 0 {
            uccs.push(set);
        } else {
            level.insert(set, partition.clone());
        }
    }

    while !level.is_empty() {
        let sets: Vec<ColumnSet> = level.keys().cloned().collect();
        let mut next = FxHashMap::default();
        for candidate in next_level(&sets, n) {
            let last = candidate.max().expect("candidates are non-empty");
            let parent_partition = &level[&candidate.without(last)];
            let partition = parent_partition.intersect(&columns[last]);
            if partition.num_non_singleton_clusters() == 0 {
                uccs.push(candidate);
            } else {
                next.insert(candidate, partition);
            }
        }
        level = next;
    }

    uccs
}

/// A minimal functional dependency `lhs -> rhs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fd {
    pub lhs: ColumnSet,
    pub rhs: usize,
}

/// Minimal functional dependencies with a single right-hand column.
///
/// The lattice is walked bottom-up. A candidate `X -> A` is minimal exactly
/// when no already-found left-hand side for `A` is a subset of `X`. Sets whose
/// partition has no non-singleton cluster are keys, so every superset yields
/// only non-minimal FDs and is not expanded.
pub fn discover_fds(columns: &[StrippedPartition]) -> Vec<Fd> {
    let n = columns.len();
    if n == 0 {
        return Vec::new();
    }
    let relation_size = columns[0].relation_size();
    let mut found: Vec<Vec<ColumnSet>> = vec![Vec::new(); n];
    let mut fds = Vec::new();

    let empty_partition = StrippedPartition::from_values(&vec![0; relation_size as usize]);
    let mut level: Vec<(ColumnSet, StrippedPartition)> =
        vec![(ColumnSet::empty(n), empty_partition)];

    while !level.is_empty() {
        for (lhs, partition) in &level {
            for rhs in 0..n {
                if lhs.contains(rhs) || found[rhs].iter().any(|f| f.is_subset_of(lhs)) {
                    continue;
                }
                let refined = partition.intersect(&columns[rhs]);
                if refined.num_clusters() == partition.num_clusters() {
                    found[rhs].push(lhs.clone());
                    fds.push(Fd {
                        lhs: lhs.clone(),
                        rhs,
                    });
                }
            }
        }

        let mut next = Vec::new();
        for (lhs, partition) in &level {
            if partition.num_non_singleton_clusters() == 0 {
                continue;
            }
            let start = lhs.max().map_or(0, |m| m + 1);
            for (c, column) in columns.iter().enumerate().skip(start) {
                let child = lhs.with(c);
                let child_partition = partition.intersect(column);
                next.push((child, child_partition));
            }
        }
        level = next;
    }

    fds
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashMap};

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, bound: u32) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as u32) % bound
        }
    }

    /// Projects every row onto `cols` and returns the tuple per row.
    fn project(data: &[Vec<u32>], cols: &[usize]) -> Vec<Vec<u32>> {
        (0..data[0].len())
            .map(|r| cols.iter().map(|&c| data[c][r]).collect())
            .collect()
    }

    fn fd_holds(data: &[Vec<u32>], lhs: &[usize], rhs: usize) -> bool {
        let mut seen: HashMap<Vec<u32>, u32> = HashMap::new();
        project(data, lhs)
            .into_iter()
            .zip(&data[rhs])
            .all(|(key, &v)| *seen.entry(key).or_insert(v) == v)
    }

    fn is_unique(data: &[Vec<u32>], cols: &[usize]) -> bool {
        let tuples = project(data, cols);
        let distinct: BTreeSet<&Vec<u32>> = tuples.iter().collect();
        distinct.len() == tuples.len()
    }

    fn brute_force_fds(data: &[Vec<u32>]) -> BTreeSet<(Vec<usize>, usize)> {
        let n = data.len();
        let mut out = BTreeSet::new();
        for mask in 0u32..(1 << n) {
            for rhs in 0..n {
                if mask & (1 << rhs) != 0 {
                    continue;
                }
                let lhs: Vec<usize> = (0..n).filter(|c| mask & (1 << c) != 0).collect();
                if !fd_holds(data, &lhs, rhs) {
                    continue;
                }
                let minimal = lhs.iter().all(|&drop| {
                    let reduced: Vec<usize> = lhs.iter().copied().filter(|&c| c != drop).collect();
                    !fd_holds(data, &reduced, rhs)
                });
                if minimal {
                    out.insert((lhs, rhs));
                }
            }
        }
        out
    }

    fn brute_force_uccs(data: &[Vec<u32>]) -> BTreeSet<Vec<usize>> {
        let n = data.len();
        let mut out = BTreeSet::new();
        for mask in 1u32..(1 << n) {
            let cols: Vec<usize> = (0..n).filter(|c| mask & (1 << c) != 0).collect();
            if !is_unique(data, &cols) {
                continue;
            }
            let minimal = cols.iter().all(|&drop| {
                let reduced: Vec<usize> = cols.iter().copied().filter(|&c| c != drop).collect();
                reduced.is_empty() || !is_unique(data, &reduced)
            });
            if minimal {
                out.insert(cols);
            }
        }
        out
    }

    fn partitions(data: &[Vec<u32>]) -> Vec<StrippedPartition> {
        data.iter()
            .map(|c| StrippedPartition::from_values(c))
            .collect()
    }

    #[test]
    fn random_tables_match_brute_force() {
        let mut rng = Lcg(7);
        for _ in 0..150 {
            let n_cols = 1 + rng.next(5) as usize;
            let n_rows = 1 + rng.next(25) as usize;
            let data: Vec<Vec<u32>> = (0..n_cols)
                .map(|_| {
                    let card = 1 + rng.next(4);
                    (0..n_rows).map(|_| rng.next(card)).collect()
                })
                .collect();

            let parts = partitions(&data);

            let fds: BTreeSet<(Vec<usize>, usize)> = discover_fds(&parts)
                .into_iter()
                .map(|fd| (fd.lhs.columns().collect(), fd.rhs))
                .collect();
            assert_eq!(fds, brute_force_fds(&data), "FD mismatch on {data:?}");

            let uccs: BTreeSet<Vec<usize>> = discover_uccs(&parts)
                .iter()
                .map(|u| u.columns().collect())
                .collect();
            assert_eq!(uccs, brute_force_uccs(&data), "UCC mismatch on {data:?}");
        }
    }
}
