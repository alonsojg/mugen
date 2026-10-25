use rayon::prelude::*;
use rustc_hash::FxHashMap;

use crate::column_set::ColumnSet;
use crate::discovery::Fd;
use crate::partition::{StrippedPartition, ValueId};

/// A lattice node: a column set with its rhs candidates (C+) and partition.
struct Vertex {
    cols: ColumnSet,
    rhs: ColumnSet,
    pli: StrippedPartition,
}

/// All vertices of one lattice level, sorted by their column lists, with an
/// index from column set to position.
struct Level {
    vertices: Vec<Vertex>,
    index: FxHashMap<ColumnSet, usize>,
}

impl Level {
    fn new(mut vertices: Vec<Vertex>) -> Self {
        vertices.sort_by_cached_key(|v| v.cols.columns().collect::<Vec<usize>>());
        let index = vertices
            .iter()
            .enumerate()
            .map(|(i, v)| (v.cols.clone(), i))
            .collect();
        Self { vertices, index }
    }

    fn vertex(&self, cols: &ColumnSet) -> Option<&Vertex> {
        self.index.get(cols).map(|&i| &self.vertices[i])
    }
}

/// Exact minimal functional dependencies using TANE's level-wise search with
/// rhs-candidate (C+) pruning. Returns the same set as
/// [`discover_fds`](crate::discover_fds).
pub fn discover_fds_tane(columns: &[StrippedPartition]) -> Vec<Fd> {
    let n = columns.len();
    if n == 0 {
        return Vec::new();
    }
    let relation_size = columns[0].relation_size();
    let empty_clusters =
        StrippedPartition::from_values(&vec![0; relation_size as usize]).num_clusters();
    let all = ColumnSet::from_columns(n, 0..n);
    let mut fds = Vec::new();

    let mut level = Level::new(
        columns
            .iter()
            .enumerate()
            .map(|(c, pli)| Vertex {
                cols: ColumnSet::singleton(n, c),
                rhs: all.clone(),
                pli: pli.clone(),
            })
            .collect(),
    );
    compute_dependencies(&mut level, None, empty_clusters, &mut fds);
    prune(&mut level, &mut fds);

    let column_tables: Vec<Vec<ValueId>> = columns.par_iter().map(|c| c.probing_table()).collect();
    while !level.vertices.is_empty() {
        let mut next = Level::new(generate_next(&level, columns, &column_tables));
        compute_dependencies(&mut next, Some(&level), empty_clusters, &mut fds);
        prune(&mut next, &mut fds);
        level = next;
    }

    fds
}

/// Intersects `parent` with `column`, matching [`StrippedPartition::intersect`]
/// but reusing the precomputed probing tables. The parent's table is built
/// lazily by the caller and shared across its children.
fn child_partition(
    parent: &StrippedPartition,
    column: &StrippedPartition,
    parent_table: &mut Option<Vec<ValueId>>,
    column_table: &[ValueId],
) -> StrippedPartition {
    if parent.size() > column.size() {
        let table = parent_table.get_or_insert_with(|| parent.probing_table());
        column.probe(table)
    } else {
        parent.probe(column_table)
    }
}

/// Joins pairs of vertices that share all but their last column. A child is
/// kept only if every one of its immediate subsets survived the previous
/// level's pruning and some rhs candidate remains. Parents are processed in
/// parallel; results keep the sequential order.
fn generate_next(
    prev: &Level,
    columns: &[StrippedPartition],
    column_tables: &[Vec<ValueId>],
) -> Vec<Vertex> {
    let keys: Vec<Vec<usize>> = prev
        .vertices
        .iter()
        .map(|v| v.cols.columns().collect())
        .collect();
    let arity = keys.first().map_or(0, Vec::len);

    (0..keys.len())
        .into_par_iter()
        .flat_map_iter(|i| {
            let parent = &prev.vertices[i];
            let mut parent_table = None;
            let mut children = Vec::new();
            for j in i + 1..keys.len() {
                if keys[i][..arity - 1] != keys[j][..arity - 1] {
                    break;
                }
                let last = keys[j][arity - 1];
                let cols = parent.cols.with(last);

                let mut rhs: Option<ColumnSet> = None;
                let mut complete = true;
                for c in cols.columns() {
                    match prev.vertex(&cols.without(c)) {
                        None => {
                            complete = false;
                            break;
                        }
                        Some(sub) => {
                            rhs = Some(match rhs {
                                None => sub.rhs.clone(),
                                Some(r) => r.intersection(&sub.rhs),
                            });
                        }
                    }
                }
                let Some(rhs) = rhs.filter(|_| complete) else {
                    continue;
                };
                if rhs.is_empty() {
                    continue;
                }
                let pli = child_partition(
                    &parent.pli,
                    &columns[last],
                    &mut parent_table,
                    &column_tables[last],
                );
                children.push(Vertex { cols, rhs, pli });
            }
            children
        })
        .collect()
}

/// Emits `X \ {A} -> A` for every rhs candidate `A` of `X` that holds, then
/// removes `A` and every column outside `X` from the candidates. `prev` is
/// `None` for level 1, where the left-hand side is the empty set.
fn compute_dependencies(
    level: &mut Level,
    prev: Option<&Level>,
    empty_clusters: usize,
    fds: &mut Vec<Fd>,
) {
    for vertex in &mut level.vertices {
        let cols = vertex.cols.clone();
        for a in cols.columns() {
            if !vertex.rhs.contains(a) {
                continue;
            }
            let lhs = cols.without(a);
            let lhs_clusters = match prev {
                None => empty_clusters,
                Some(p) => p
                    .vertex(&lhs)
                    .expect("subsets of generated vertices are present")
                    .pli
                    .num_clusters(),
            };
            if lhs_clusters == vertex.pli.num_clusters() {
                fds.push(Fd { lhs, rhs: a });
                vertex.rhs = vertex.rhs.intersection(&cols).without(a);
            }
        }
    }
}

/// For a key `X` (no duplicate rows under `X`), emits `X -> A` for each rhs
/// candidate `A` outside `X` that also remains a candidate in every sibling
/// `X \ {B} ∪ {A}`. Keys stay in the level, with their candidates restricted to
/// their own columns, so supersets can still be generated and checked. Vertices
/// left without candidates are removed. Decisions read the unpruned level.
fn prune(level: &mut Level, fds: &mut Vec<Fd>) {
    let mut new_rhs = Vec::with_capacity(level.vertices.len());
    for v in &level.vertices {
        if v.pli.nep() == 0 {
            for a in v.rhs.columns().filter(|a| !v.cols.contains(*a)) {
                let holds = v.cols.columns().all(|b| {
                    level
                        .vertex(&v.cols.with(a).without(b))
                        .is_some_and(|s| s.rhs.contains(a))
                });
                if holds {
                    fds.push(Fd {
                        lhs: v.cols.clone(),
                        rhs: a,
                    });
                }
            }
            new_rhs.push(v.rhs.intersection(&v.cols));
        } else {
            new_rhs.push(v.rhs.clone());
        }
    }

    for (v, rhs) in level.vertices.iter_mut().zip(new_rhs) {
        v.rhs = rhs;
    }
    level.vertices.retain(|v| !v.rhs.is_empty());
    level.index = level
        .vertices
        .iter()
        .enumerate()
        .map(|(i, v)| (v.cols.clone(), i))
        .collect();
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

    #[test]
    fn matches_level_wise_discovery_on_random_tables() {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
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
                normalize(&discover_fds_tane(&cols)),
                normalize(&discover_fds(&cols)),
                "rows: {rows:?}"
            );
        }
    }

    #[test]
    fn constant_column_yields_empty_lhs_fd() {
        let rows = vec![vec![7, 1], vec![7, 2], vec![7, 3]];
        let cols = partitions(&rows, 2);
        assert_eq!(normalize(&discover_fds_tane(&cols)), vec![(Vec::new(), 0)]);
    }
}
