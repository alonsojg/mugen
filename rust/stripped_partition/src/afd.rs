use rustc_hash::FxHashMap;

use crate::column_set::ColumnSet;
use crate::discovery::Fd;
use crate::partition::{RowId, StrippedPartition, ValueId, SINGLETON};

/// Error measures for approximate functional dependencies, as in Desbordante's
/// `AFDMetricCalculator`. Every measure yields 0 for an exact FD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfdMeasure {
    G1,
    G2,
    G3,
    Pdep,
    Tau,
    Rho,
    Fi,
    MuPlus,
}

/// An approximate FD together with its error under the chosen measure.
#[derive(Clone, Debug, PartialEq)]
pub struct Afd {
    pub fd: Fd,
    pub error: f64,
}

/// A lattice node: column set, remaining rhs candidates (C+), whether the
/// columns are still an approximate key candidate, and the partition.
struct Vertex {
    cols: ColumnSet,
    rhs: ColumnSet,
    key: bool,
    pli: StrippedPartition,
}

struct Level {
    vertices: Vec<Vertex>,
    index: FxHashMap<ColumnSet, usize>,
}

impl Level {
    fn new(mut vertices: Vec<Vertex>) -> Self {
        vertices.sort_by_cached_key(|v| v.cols.columns().collect::<Vec<usize>>());
        let index = Self::build_index(&vertices);
        Self { vertices, index }
    }

    fn build_index(vertices: &[Vertex]) -> FxHashMap<ColumnSet, usize> {
        vertices
            .iter()
            .enumerate()
            .map(|(i, v)| (v.cols.clone(), i))
            .collect()
    }

    fn vertex(&self, cols: &ColumnSet) -> Option<&Vertex> {
        self.index.get(cols).map(|&i| &self.vertices[i])
    }
}

/// Discovers minimal approximate FDs whose error under `measure` is at most
/// `max_error`, using TANE's level-wise search. With `max_error == 0` and
/// [`AfdMeasure::G3`] this finds the exact minimal FDs.
pub fn discover_afds_tane(
    columns: &[StrippedPartition],
    measure: AfdMeasure,
    max_error: f64,
) -> Vec<Afd> {
    let n = columns.len();
    if n == 0 {
        return Vec::new();
    }
    let relation_size = columns[0].relation_size();
    if relation_size <= 1 {
        return (0..n)
            .map(|a| Afd {
                fd: Fd {
                    lhs: ColumnSet::empty(n),
                    rhs: a,
                },
                error: 0.0,
            })
            .collect();
    }

    let pairs = pair_count(relation_size);
    let all = ColumnSet::from_columns(n, 0..n);
    let mut out = Vec::new();

    let mut level1: Vec<Vertex> = columns
        .iter()
        .enumerate()
        .map(|(c, pli)| Vertex {
            cols: ColumnSet::singleton(n, c),
            rhs: all.clone(),
            key: true,
            pli: pli.clone(),
        })
        .collect();

    // Level 1 is handled separately, with the empty set as its left-hand side.
    let mut constant_rhs = ColumnSet::empty(n);
    for (a, v) in level1.iter_mut().enumerate() {
        let error = zero_ary_error(measure, &v.pli);
        if error <= max_error {
            out.push(Afd {
                fd: Fd {
                    lhs: ColumnSet::empty(n),
                    rhs: a,
                },
                error,
            });
            constant_rhs = constant_rhs.with(a);
            v.rhs = v.rhs.without(a);
            if error == 0.0 {
                v.rhs = ColumnSet::empty(n);
            }
        }
    }
    let not_constant = ColumnSet::from_columns(n, (0..n).filter(|a| !constant_rhs.contains(*a)));
    for v in &mut level1 {
        v.rhs = v.rhs.intersection(&not_constant);
    }
    for v in &mut level1 {
        let nep = v.pli.nep();
        if nep as f64 / pairs <= max_error {
            v.key = false;
            if nep == 0 {
                let a = v.cols.max().expect("level 1 vertices hold one column");
                for b in v.rhs.columns().filter(|b| *b != a) {
                    out.push(Afd {
                        fd: Fd {
                            lhs: v.cols.clone(),
                            rhs: b,
                        },
                        error: 0.0,
                    });
                }
                v.rhs = v.rhs.intersection(&v.cols);
            }
        }
    }

    let mut prev = Level::new(level1.into_iter().filter(|v| !v.rhs.is_empty()).collect());
    loop {
        let mut next = Level::new(generate_next(&prev, columns));
        if next.vertices.is_empty() {
            break;
        }
        compute_dependencies(&mut next, &prev, columns, measure, max_error, &mut out);
        prune(&mut next, pairs, max_error, &mut out);
        prev = next;
    }

    out
}

/// Joins pairs of vertices that share all but their last column. A child is
/// kept only if all of its immediate subsets survived pruning and some rhs
/// candidate remains. It is an approximate key candidate only if all of them
/// still are.
fn generate_next(prev: &Level, columns: &[StrippedPartition]) -> Vec<Vertex> {
    let keys: Vec<Vec<usize>> = prev
        .vertices
        .iter()
        .map(|v| v.cols.columns().collect())
        .collect();
    let arity = keys.first().map_or(0, Vec::len);
    let mut next = Vec::new();

    for i in 0..keys.len() {
        for j in i + 1..keys.len() {
            if keys[i][..arity - 1] != keys[j][..arity - 1] {
                break;
            }
            let parent = &prev.vertices[i];
            let last = keys[j][arity - 1];
            let cols = parent.cols.with(last);

            let mut rhs: Option<ColumnSet> = None;
            let mut key = true;
            let mut complete = true;
            for c in cols.columns() {
                let Some(sub) = prev.vertex(&cols.without(c)) else {
                    complete = false;
                    break;
                };
                key &= sub.key;
                rhs = Some(match rhs {
                    None => sub.rhs.clone(),
                    Some(r) => r.intersection(&sub.rhs),
                });
            }
            let Some(rhs) = rhs.filter(|r| complete && !r.is_empty()) else {
                continue;
            };
            let pli = parent.pli.intersect(&columns[last]);
            next.push(Vertex {
                cols,
                rhs,
                key,
                pli,
            });
        }
    }

    next
}

/// Emits `X \ {A} -> A` for every rhs candidate `A` of `X` whose error is
/// within the threshold. The candidate is removed. An exact FD also removes
/// every column outside `X`.
fn compute_dependencies(
    level: &mut Level,
    prev: &Level,
    columns: &[StrippedPartition],
    measure: AfdMeasure,
    max_error: f64,
    out: &mut Vec<Afd>,
) {
    for v in &mut level.vertices {
        let cols = v.cols.clone();
        for a in cols.columns() {
            if !v.rhs.contains(a) {
                continue;
            }
            let lhs = cols.without(a);
            let parent = prev
                .vertex(&lhs)
                .expect("subsets of generated vertices are present");
            let error = fd_error(measure, &parent.pli, &columns[a], &v.pli);
            if error <= max_error {
                out.push(Afd {
                    fd: Fd { lhs, rhs: a },
                    error,
                });
                v.rhs = v.rhs.without(a);
                if error == 0.0 {
                    v.rhs = v.rhs.intersection(&cols);
                }
            }
        }
    }
}

/// For an approximate key `X`, emits `X -> A` (error 0) for each candidate
/// `A` outside `X` that every sibling `X \ {B} ∪ {A}` also still lists. The
/// key flag is cleared once the UCC error is within the threshold. Keys stay
/// in the level with their candidates unchanged. Vertices without candidates
/// are removed. Decisions read the unpruned level.
fn prune(level: &mut Level, pairs: f64, max_error: f64, out: &mut Vec<Afd>) {
    let exact = max_error == 0.0;
    let mut new_rhs = Vec::with_capacity(level.vertices.len());
    let mut clear_key = vec![false; level.vertices.len()];
    for (i, v) in level.vertices.iter().enumerate() {
        if !v.key || v.pli.nep() as f64 / pairs > max_error {
            new_rhs.push(v.rhs.clone());
            continue;
        }
        clear_key[i] = true;
        if v.pli.nep() == 0 {
            let mut rhs = v.rhs.clone();
            for a in v.rhs.columns().filter(|a| !v.cols.contains(*a)) {
                let holds = v.cols.columns().all(|b| {
                    level
                        .vertex(&v.cols.with(a).without(b))
                        .is_some_and(|s| s.rhs.contains(a))
                });
                if holds {
                    out.push(Afd {
                        fd: Fd {
                            lhs: v.cols.clone(),
                            rhs: a,
                        },
                        error: 0.0,
                    });
                    rhs = rhs.without(a);
                }
            }
            new_rhs.push(if exact {
                v.rhs.intersection(&v.cols)
            } else {
                rhs
            });
        } else {
            new_rhs.push(v.rhs.clone());
        }
    }

    for ((v, clear), rhs) in level.vertices.iter_mut().zip(clear_key).zip(new_rhs) {
        v.rhs = rhs;
        if clear {
            v.key = false;
        }
    }
    level.vertices.retain(|v| !v.rhs.is_empty());
    level.index = Level::build_index(&level.vertices);
}

fn pair_count(rows: u32) -> f64 {
    let n = f64::from(rows);
    n * (n - 1.0) / 2.0
}

/// Error of the empty-lhs FD `∅ -> A`.
fn zero_ary_error(measure: AfdMeasure, a: &StrippedPartition) -> f64 {
    let n = f64::from(a.relation_size());
    match measure {
        AfdMeasure::G1 => 1.0 - a.nep() as f64 / pair_count(a.relation_size()),
        AfdMeasure::G3 => {
            let largest = a.clusters().map(<[RowId]>::len).max().unwrap_or(0).max(1);
            1.0 - largest as f64 / n
        }
        AfdMeasure::Rho => {
            let k = a.num_clusters() as f64;
            (k - 1.0) / k
        }
        AfdMeasure::Pdep => 1.0 - pdep_self(a),
        AfdMeasure::G2 | AfdMeasure::Fi | AfdMeasure::Tau | AfdMeasure::MuPlus => {
            if is_constant(a) {
                0.0
            } else {
                1.0
            }
        }
    }
}

/// Error of the FD whose lhs has partition `lhs`, rhs has partition `rhs`,
/// and whose union has partition `joint`.
fn fd_error(
    measure: AfdMeasure,
    lhs: &StrippedPartition,
    rhs: &StrippedPartition,
    joint: &StrippedPartition,
) -> f64 {
    let n = f64::from(rhs.relation_size());
    match measure {
        AfdMeasure::G1 => (lhs.nep() - joint.nep()) as f64 / pair_count(rhs.relation_size()),
        AfdMeasure::G2 => {
            let probe = rhs.probing_table();
            let mut scratch = Scratch::new(rhs);
            let mut impure = 0u64;
            for c in lhs.clusters() {
                let singles = scratch.count(c, &probe);
                if singles > 0 || scratch.touched.len() > 1 {
                    impure += c.len() as u64;
                }
                scratch.reset();
            }
            impure as f64 / n
        }
        AfdMeasure::G3 => {
            let probe = rhs.probing_table();
            let mut scratch = Scratch::new(rhs);
            let mut kept = f64::from(lhs.relation_size() - lhs.size());
            for c in lhs.clusters() {
                scratch.count(c, &probe);
                kept += scratch.frequencies().max().unwrap_or(0).max(1) as f64;
                scratch.reset();
            }
            1.0 - kept / n
        }
        AfdMeasure::Pdep => 1.0 - pdep(lhs, rhs),
        AfdMeasure::Tau => 1.0 - tau(lhs, rhs),
        AfdMeasure::Rho => 1.0 - lhs.num_clusters() as f64 / joint.num_clusters() as f64,
        AfdMeasure::Fi => 1.0 - fi(lhs, rhs),
        AfdMeasure::MuPlus => 1.0 - mu_plus(lhs, rhs),
    }
}

fn is_constant(pli: &StrippedPartition) -> bool {
    let n = pli.relation_size();
    n <= 1 || (pli.num_non_singleton_clusters() == 1 && pli.size() == n)
}

/// Probability that two rows agreeing on `y` also agree on `y` (`pdep(∅ -> Y)`).
fn pdep_self(y: &StrippedPartition) -> f64 {
    let n = f64::from(y.relation_size());
    let squares: u64 = y.clusters().map(|c| (c.len() as u64).pow(2)).sum();
    let singles = f64::from(y.relation_size() - y.size());
    (squares as f64 + singles) / (n * n)
}

/// Probabilistic dependency `pdep(X -> Y)`.
fn pdep(lhs: &StrippedPartition, rhs: &StrippedPartition) -> f64 {
    let n = f64::from(rhs.relation_size());
    let probe = rhs.probing_table();
    let mut scratch = Scratch::new(rhs);
    let mut sum = f64::from(rhs.relation_size() - lhs.size());
    for c in lhs.clusters() {
        let size = c.len() as f64;
        let singles = scratch.count(c, &probe);
        let squares: f64 = scratch.frequencies().map(|k| f64::from(k).powi(2)).sum();
        sum += (squares + f64::from(singles)) / size;
        scratch.reset();
    }
    sum / n
}

fn tau(lhs: &StrippedPartition, rhs: &StrippedPartition) -> f64 {
    let py = pdep_self(rhs);
    if py == 1.0 {
        return 1.0;
    }
    (pdep(lhs, rhs) - py) / (1.0 - py)
}

fn mu_plus(lhs: &StrippedPartition, rhs: &StrippedPartition) -> f64 {
    let py = pdep_self(rhs);
    if py == 1.0 {
        return 1.0;
    }
    let n = f64::from(rhs.relation_size());
    let k = lhs.num_clusters() as f64;
    if k == n {
        return 1.0;
    }
    let pxy = pdep(lhs, rhs);
    (1.0 - (1.0 - pxy) / (1.0 - py) * (n - 1.0) / (n - k)).max(0.0)
}

/// Fraction of the entropy of `Y` explained by `X`.
fn fi(lhs: &StrippedPartition, rhs: &StrippedPartition) -> f64 {
    if rhs.num_clusters() < 2 {
        return 0.0;
    }
    let n = f64::from(rhs.relation_size());
    let probe = rhs.probing_table();
    let mut scratch = Scratch::new(rhs);
    let mut conditional = 0.0;
    for c in lhs.clusters() {
        let log_size = (c.len() as f64).ln();
        let singles = scratch.count(c, &probe);
        for k in scratch.frequencies() {
            let s = f64::from(k);
            conditional -= s * (s.ln() - log_size);
        }
        conditional += f64::from(singles) * log_size;
        scratch.reset();
    }
    conditional /= n;
    let hy = rhs.entropy();
    (hy - conditional) / hy
}

/// Counts RHS values inside one LHS cluster, reusing buffers across clusters.
struct Scratch {
    counts: Vec<u32>,
    touched: Vec<ValueId>,
}

impl Scratch {
    fn new(rhs: &StrippedPartition) -> Self {
        Self {
            counts: vec![0; rhs.num_non_singleton_clusters() + 1],
            touched: Vec::new(),
        }
    }

    /// Counts the non-singleton RHS values among `rows` and returns how many
    /// rows carry a singleton RHS value.
    fn count(&mut self, rows: &[RowId], probe: &[ValueId]) -> u32 {
        let mut singles = 0;
        for &row in rows {
            let y = probe[row as usize];
            if y == SINGLETON {
                singles += 1;
                continue;
            }
            if self.counts[y as usize] == 0 {
                self.touched.push(y);
            }
            self.counts[y as usize] += 1;
        }
        self.touched.sort_unstable();
        singles
    }

    fn frequencies(&self) -> impl Iterator<Item = u32> + '_ {
        self.touched.iter().map(|&y| self.counts[y as usize])
    }

    fn reset(&mut self) {
        for &y in &self.touched {
            self.counts[y as usize] = 0;
        }
        self.touched.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::Fd;
    use crate::tane::discover_fds_tane;

    fn partitions(rows: &[Vec<u32>], num_columns: usize) -> Vec<StrippedPartition> {
        (0..num_columns)
            .map(|c| {
                let values: Vec<u32> = rows.iter().map(|r| r[c]).collect();
                StrippedPartition::from_values(&values)
            })
            .collect()
    }

    /// Naive G3 error of `lhs -> rhs`: the fraction of rows to delete so the
    /// FD holds.
    fn naive_g3(rows: &[Vec<u32>], lhs: &[usize], rhs: usize) -> f64 {
        let mut groups: std::collections::HashMap<Vec<u32>, std::collections::HashMap<u32, usize>> =
            std::collections::HashMap::new();
        for r in rows {
            let key: Vec<u32> = lhs.iter().map(|&c| r[c]).collect();
            *groups.entry(key).or_default().entry(r[rhs]).or_default() += 1;
        }
        let kept: usize = groups
            .values()
            .map(|g| g.values().copied().max().unwrap_or(0))
            .sum();
        1.0 - kept as f64 / rows.len() as f64
    }

    fn normalize(afds: &[Afd]) -> Vec<(Vec<usize>, usize)> {
        let mut v: Vec<(Vec<usize>, usize)> = afds
            .iter()
            .map(|a| (a.fd.lhs.columns().collect(), a.fd.rhs))
            .collect();
        v.sort();
        v
    }

    /// Minimal `lhs -> rhs` with `naive_g3 <= max`, by enumerating all lhs.
    fn brute_force_minimal_g3(
        rows: &[Vec<u32>],
        num_columns: usize,
        max: f64,
    ) -> Vec<(Vec<usize>, usize)> {
        let mut out = Vec::new();
        for rhs in 0..num_columns {
            let others: Vec<usize> = (0..num_columns).filter(|&c| c != rhs).collect();
            let mut holds: Vec<Vec<usize>> = Vec::new();
            for mask in 0u32..(1 << others.len()) {
                let lhs: Vec<usize> = others
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| mask & (1 << i) != 0)
                    .map(|(_, &c)| c)
                    .collect();
                if naive_g3(rows, &lhs, rhs) <= max + 1e-12 {
                    holds.push(lhs);
                }
            }
            for lhs in &holds {
                let minimal = !holds
                    .iter()
                    .any(|other| other.len() < lhs.len() && other.iter().all(|c| lhs.contains(c)));
                if minimal {
                    out.push((lhs.clone(), rhs));
                }
            }
        }
        out.sort();
        out
    }

    fn random_rows(next: &mut impl FnMut(u32) -> u32) -> (Vec<Vec<u32>>, usize) {
        let num_columns = 2 + next(4) as usize;
        let num_rows = 1 + next(12) as usize;
        let rows = (0..num_rows)
            .map(|_| (0..num_columns).map(|_| next(3)).collect())
            .collect();
        (rows, num_columns)
    }

    fn lcg(seed: u64) -> impl FnMut(u32) -> u32 {
        let mut state = seed;
        move |bound: u32| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) as u32) % bound
        }
    }

    #[test]
    fn g3_matches_brute_force_minimal_afds() {
        let mut next = lcg(0xDEAD_BEEF_0BAD_F00D);
        for _ in 0..200 {
            let (rows, num_columns) = random_rows(&mut next);
            let cols = partitions(&rows, num_columns);
            for max in [0.0, 0.2, 0.34, 0.5] {
                let found = discover_afds_tane(&cols, AfdMeasure::G3, max);
                for afd in &found {
                    let lhs: Vec<usize> = afd.fd.lhs.columns().collect();
                    let expected = naive_g3(&rows, &lhs, afd.fd.rhs);
                    assert!((afd.error - expected).abs() < 1e-9, "rows: {rows:?}");
                    assert!(afd.error <= max + 1e-12);
                }
                assert_eq!(
                    normalize(&found),
                    brute_force_minimal_g3(&rows, num_columns, max),
                    "rows: {rows:?}, max: {max}"
                );
            }
        }
    }

    #[test]
    fn zero_threshold_matches_exact_tane_for_every_measure() {
        let mut next = lcg(0x0123_4567_89AB_CDEF);
        for _ in 0..100 {
            let (rows, num_columns) = random_rows(&mut next);
            let cols = partitions(&rows, num_columns);
            let exact: Vec<(Vec<usize>, usize)> = {
                let mut v: Vec<(Vec<usize>, usize)> = discover_fds_tane(&cols)
                    .iter()
                    .map(|fd: &Fd| (fd.lhs.columns().collect(), fd.rhs))
                    .collect();
                v.sort();
                v
            };
            for measure in [
                AfdMeasure::G1,
                AfdMeasure::G3,
                AfdMeasure::Pdep,
                AfdMeasure::Tau,
                AfdMeasure::Rho,
                AfdMeasure::MuPlus,
            ] {
                let found = discover_afds_tane(&cols, measure, 0.0);
                assert!(found.iter().all(|a| a.error == 0.0), "{measure:?}");
                assert_eq!(normalize(&found), exact, "{measure:?}, rows: {rows:?}");
            }
        }
    }

    #[test]
    fn errors_stay_in_unit_interval() {
        let mut next = lcg(0x5555_AAAA_1234_5678);
        let measures = [
            AfdMeasure::G1,
            AfdMeasure::G2,
            AfdMeasure::G3,
            AfdMeasure::Pdep,
            AfdMeasure::Tau,
            AfdMeasure::Rho,
            AfdMeasure::Fi,
            AfdMeasure::MuPlus,
        ];
        for _ in 0..100 {
            let (rows, num_columns) = random_rows(&mut next);
            let cols = partitions(&rows, num_columns);
            for measure in measures {
                for afd in discover_afds_tane(&cols, measure, 1.0) {
                    assert!(
                        (-1e-9..=1.0 + 1e-9).contains(&afd.error),
                        "{measure:?}: {afd:?}, rows: {rows:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn g3_hand_computed_error() {
        // a -> b fails on one of the three rows with a = 1 (kept 2 of 4).
        let rows = vec![vec![1, 5], vec![1, 5], vec![1, 6], vec![2, 7]];
        let cols = partitions(&rows, 2);
        let found = discover_afds_tane(&cols, AfdMeasure::G3, 0.25);
        let ab = found
            .iter()
            .find(|a| a.fd.lhs.columns().eq([0]) && a.fd.rhs == 1)
            .expect("0 -> 1 is within 0.25");
        assert!((ab.error - 0.25).abs() < 1e-12);
    }
}
