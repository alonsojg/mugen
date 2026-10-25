pub type RowId = u32;
pub type ValueId = u32;

/// Probing-table value for rows that belong to no non-singleton cluster.
pub const SINGLETON: ValueId = 0;

/// Reusable buffers for [`StrippedPartition::probe_with`]. Counts are zeroed
/// again after every cluster, so the buffers stay clean between calls.
#[derive(Debug, Default)]
pub struct ProbeScratch {
    counts: Vec<u32>,
    cursor: Vec<usize>,
}

impl ProbeScratch {
    /// Buffers large enough for probing tables whose values are below `key_space`.
    pub fn with_key_space(key_space: usize) -> Self {
        Self {
            counts: vec![0; key_space],
            cursor: vec![0; key_space],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrippedPartition {
    rows: Vec<RowId>,
    offsets: Vec<u32>,
    relation_size: u32,
}

impl StrippedPartition {
    /// Builds the partition of one column. Value ids should be dense
    /// (`0..k`), because scratch memory is proportional to the largest id.
    pub fn from_values(values: &[ValueId]) -> Self {
        let relation_size = values.len();
        assert!(
            relation_size <= u32::MAX as usize,
            "relation too large for u32 row ids"
        );

        let key_space = values.iter().copied().max().map_or(0, |m| m as usize + 1);
        let mut counts = vec![0u32; key_space];
        for &v in values {
            counts[v as usize] += 1;
        }

        let mut slot = vec![u32::MAX; key_space];
        let mut sizes: Vec<u32> = Vec::new();
        for &v in values {
            let v = v as usize;
            if counts[v] > 1 && slot[v] == u32::MAX {
                slot[v] = sizes.len() as u32;
                sizes.push(counts[v]);
            }
        }

        let mut offsets = Vec::with_capacity(sizes.len() + 1);
        offsets.push(0u32);
        for &s in &sizes {
            let last = *offsets.last().expect("offsets is never empty");
            offsets.push(last + s);
        }

        let total = *offsets.last().expect("offsets is never empty") as usize;
        let mut rows = vec![0 as RowId; total];
        let mut cursor: Vec<u32> = offsets[..sizes.len()].to_vec();
        for (row, &v) in values.iter().enumerate() {
            let s = slot[v as usize];
            if s != u32::MAX {
                rows[cursor[s as usize] as usize] = row as RowId;
                cursor[s as usize] += 1;
            }
        }

        Self {
            rows,
            offsets,
            relation_size: relation_size as u32,
        }
    }

    /// Number of rows that appear in non-singleton clusters.
    pub fn size(&self) -> u32 {
        self.rows.len() as u32
    }

    pub fn relation_size(&self) -> u32 {
        self.relation_size
    }

    /// Number of non-singleton clusters.
    pub fn num_non_singleton_clusters(&self) -> usize {
        self.offsets.len() - 1
    }

    /// Number of clusters including singletons.
    pub fn num_clusters(&self) -> usize {
        self.num_non_singleton_clusters() + (self.relation_size - self.size()) as usize
    }

    pub fn clusters(&self) -> impl Iterator<Item = &[RowId]> + '_ {
        self.offsets
            .windows(2)
            .map(move |w| &self.rows[w[0] as usize..w[1] as usize])
    }

    /// Number of unordered row pairs that agree on the partitioned attributes.
    pub fn nep(&self) -> u64 {
        self.clusters()
            .map(|c| {
                let n = c.len() as u64;
                n * (n - 1) / 2
            })
            .sum()
    }

    pub fn entropy(&self) -> f64 {
        if self.relation_size == 0 {
            return 0.0;
        }
        let n = self.relation_size as f64;
        let key_gap: f64 = self
            .clusters()
            .map(|c| c.len() as f64 * (c.len() as f64).ln())
            .sum();
        n.ln() - key_gap / n
    }

    pub fn gini_impurity(&self) -> f64 {
        if self.relation_size == 0 {
            return 0.0;
        }
        let n = self.relation_size as f64;
        let squares: f64 = self.clusters().map(|c| (c.len() as f64).powi(2)).sum();
        let singletons = (self.relation_size - self.size()) as f64;
        1.0 - (squares + singletons) / n.powi(2)
    }

    pub fn inverted_entropy(&self) -> f64 {
        if self.gini_impurity() == 0.0 {
            return 0.0;
        }
        let n = self.relation_size as f64;
        self.clusters()
            .map(|c| {
                let p = c.len() as f64 / n;
                -(1.0 - p) * (1.0 - p).ln()
            })
            .sum()
    }

    /// Probing table: row -> 1-based cluster id, or [`SINGLETON`].
    pub fn probing_table(&self) -> Vec<ValueId> {
        let mut table = vec![SINGLETON; self.relation_size as usize];
        for (id, cluster) in self.clusters().enumerate() {
            for &row in cluster {
                table[row as usize] = id as ValueId + 1;
            }
        }
        table
    }

    /// Refines this partition by the probing table of another one.
    pub fn probe(&self, probing_table: &[ValueId]) -> Self {
        let key_space = probing_table
            .iter()
            .copied()
            .max()
            .map_or(0, |m| m as usize + 1);
        self.probe_with(probing_table, &mut ProbeScratch::with_key_space(key_space))
    }

    /// Approximate heap bytes held by the row and offset arrays.
    pub fn memory_bytes(&self) -> usize {
        (self.rows.len() + self.offsets.len()) * std::mem::size_of::<u32>()
    }

    /// Number of row pairs that agree on this partition and on `probing_table`.
    ///
    /// Equal to `self.probe_with(probing_table, scratch).nep()`, but without
    /// materializing the refined partition.
    pub fn joint_nep_with(&self, probing_table: &[ValueId], scratch: &mut ProbeScratch) -> u64 {
        assert_eq!(probing_table.len(), self.relation_size as usize);
        let counts = &mut scratch.counts;
        let mut touched: Vec<ValueId> = Vec::new();
        let mut pairs = 0u64;

        for cluster in self.clusters() {
            for &row in cluster {
                let v = probing_table[row as usize];
                if v == SINGLETON {
                    continue;
                }
                if counts[v as usize] == 0 {
                    touched.push(v);
                }
                counts[v as usize] += 1;
            }
            for &v in &touched {
                let c = u64::from(counts[v as usize]);
                pairs += c * (c - 1) / 2;
                counts[v as usize] = 0;
            }
            touched.clear();
        }
        pairs
    }

    /// Like [`probe`](Self::probe), reusing `scratch` across calls.
    ///
    /// `scratch` must have been sized with [`ProbeScratch::with_key_space`] for
    /// a key space at least as large as the values in `probing_table`.
    pub fn probe_with(&self, probing_table: &[ValueId], scratch: &mut ProbeScratch) -> Self {
        assert_eq!(probing_table.len(), self.relation_size as usize);

        let counts = &mut scratch.counts;
        let cursor = &mut scratch.cursor;
        let mut touched: Vec<ValueId> = Vec::new();
        let mut out_rows: Vec<RowId> = Vec::new();
        let mut out_ranges: Vec<(usize, usize)> = Vec::new();

        for cluster in self.clusters() {
            for &row in cluster {
                let v = probing_table[row as usize];
                if v == SINGLETON {
                    continue;
                }
                if counts[v as usize] == 0 {
                    touched.push(v);
                }
                counts[v as usize] += 1;
            }

            let mut out_len = out_rows.len();
            for &v in &touched {
                let count = counts[v as usize] as usize;
                if count > 1 {
                    cursor[v as usize] = out_len;
                    out_ranges.push((out_len, count));
                    out_len += count;
                }
            }
            out_rows.resize(out_len, 0);

            for &row in cluster {
                let v = probing_table[row as usize];
                if v != SINGLETON && counts[v as usize] > 1 {
                    out_rows[cursor[v as usize]] = row;
                    cursor[v as usize] += 1;
                }
            }

            for &v in &touched {
                counts[v as usize] = 0;
            }
            touched.clear();
        }

        out_ranges.sort_unstable_by_key(|&(start, _)| out_rows[start]);

        let mut rows = Vec::with_capacity(out_rows.len());
        let mut offsets = Vec::with_capacity(out_ranges.len() + 1);
        offsets.push(0u32);
        for (start, len) in out_ranges {
            rows.extend_from_slice(&out_rows[start..start + len]);
            offsets.push(rows.len() as u32);
        }

        Self {
            rows,
            offsets,
            relation_size: self.relation_size,
        }
    }

    /// Partition of the union of the two attribute sets.
    pub fn intersect(&self, other: &Self) -> Self {
        assert_eq!(self.relation_size, other.relation_size);
        if self.size() > other.size() {
            other.probe(&self.probing_table())
        } else {
            self.probe(&other.probing_table())
        }
    }

    /// `self -> rhs` holds iff the lhs partition refines the rhs partition,
    /// i.e. adding the rhs attribute does not split any lhs class.
    pub fn fd_holds(&self, rhs: &Self) -> bool {
        self.num_clusters() == self.intersect(rhs).num_clusters()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn dense(keys: &[(u32, u32)]) -> Vec<ValueId> {
        let mut ids: HashMap<(u32, u32), ValueId> = HashMap::new();
        keys.iter()
            .map(|k| {
                let next = ids.len() as ValueId;
                *ids.entry(*k).or_insert(next)
            })
            .collect()
    }

    #[test]
    fn joint_nep_matches_probe_nep() {
        let values: Vec<u32> = vec![0, 0, 0, 1, 1, 2, 2, 2, 2, 3];
        let other: Vec<u32> = vec![5, 5, 6, 6, 6, 5, 5, 5, 7, 7];
        let part = StrippedPartition::from_values(&values);
        let table = StrippedPartition::from_values(&other).probing_table();
        let mut scratch = ProbeScratch::with_key_space(8);
        assert_eq!(
            part.joint_nep_with(&table, &mut scratch),
            part.probe(&table).nep()
        );
    }

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

    #[test]
    fn from_values_strips_singletons_and_orders_by_first_row() {
        let p = StrippedPartition::from_values(&[0, 1, 0, 2, 1, 0]);
        let clusters: Vec<Vec<RowId>> = p.clusters().map(|c| c.to_vec()).collect();
        assert_eq!(clusters, vec![vec![0, 2, 5], vec![1, 4]]);
        assert_eq!(p.size(), 5);
        assert_eq!(p.relation_size(), 6);
        assert_eq!(p.num_clusters(), 3);
        assert_eq!(p.nep(), 3 + 1);
    }

    #[test]
    fn all_unique_and_constant_columns() {
        let unique = StrippedPartition::from_values(&[0, 1, 2]);
        assert_eq!(unique.num_non_singleton_clusters(), 0);
        assert_eq!(unique.num_clusters(), 3);
        assert_eq!(unique.gini_impurity(), 1.0 - 3.0 / 9.0);
        assert_eq!(unique.inverted_entropy(), 0.0);

        let constant = StrippedPartition::from_values(&[0, 0, 0]);
        assert_eq!(constant.num_clusters(), 1);
        assert_eq!(constant.gini_impurity(), 0.0);
        assert_eq!(constant.inverted_entropy(), 0.0);
        assert!(constant.entropy().abs() < 1e-12);
    }

    #[test]
    fn empty_relation_is_well_defined() {
        let p = StrippedPartition::from_values(&[]);
        assert_eq!(p.num_clusters(), 0);
        assert_eq!(p.entropy(), 0.0);
        assert_eq!(p.gini_impurity(), 0.0);
    }

    #[test]
    fn probing_table_matches_clusters() {
        let p = StrippedPartition::from_values(&[0, 1, 0, 2, 1, 0]);
        assert_eq!(p.probing_table(), vec![1, 2, 1, 0, 2, 1]);
    }

    #[test]
    fn intersect_matches_partition_of_tuples() {
        let a = [0, 0, 1, 1, 0, 2, 2, 0];
        let b = [0, 1, 0, 1, 0, 0, 1, 0];
        let pa = StrippedPartition::from_values(&a);
        let pb = StrippedPartition::from_values(&b);
        let pairs: Vec<(u32, u32)> = a.iter().copied().zip(b.iter().copied()).collect();
        let expected = StrippedPartition::from_values(&dense(&pairs));
        assert_eq!(pa.intersect(&pb), expected);
        assert_eq!(pb.intersect(&pa), expected);
    }

    #[test]
    fn fd_holds_detects_refinement() {
        let a = StrippedPartition::from_values(&[0, 0, 1, 1, 2]);
        let b_determined = StrippedPartition::from_values(&[5, 5, 6, 6, 7]);
        let b_split = StrippedPartition::from_values(&[5, 6, 6, 6, 7]);
        assert!(a.fd_holds(&b_determined));
        assert!(!a.fd_holds(&b_split));
    }

    #[test]
    fn random_intersections_match_naive_grouping() {
        let mut rng = Lcg(42);
        for _ in 0..200 {
            let n = 1 + rng.next(60) as usize;
            let a: Vec<u32> = (0..n).map(|_| rng.next(5)).collect();
            let b: Vec<u32> = (0..n).map(|_| rng.next(4)).collect();
            let c: Vec<u32> = (0..n).map(|_| rng.next(3)).collect();
            let pa = StrippedPartition::from_values(&a);
            let pb = StrippedPartition::from_values(&b);
            let pc = StrippedPartition::from_values(&c);

            let ab: Vec<(u32, u32)> = a.iter().copied().zip(b.iter().copied()).collect();
            let abc: Vec<(u32, u32)> = ab
                .iter()
                .copied()
                .zip(c.iter().copied())
                .map(|((x, y), z)| (x * 16 + y, z))
                .collect();

            let pab = pa.intersect(&pb);
            assert_eq!(pab, StrippedPartition::from_values(&dense(&ab)));
            assert_eq!(
                pab.intersect(&pc),
                StrippedPartition::from_values(&dense(&abc))
            );
            assert_eq!(pb.intersect(&pa), pab);
        }
    }

    #[test]
    fn statistics_match_hand_computed_values() {
        let p = StrippedPartition::from_values(&[0, 0, 0, 1, 1, 2]);
        let n = 6.0f64;
        assert_eq!(p.nep(), 3 + 1);
        let expected_entropy = n.ln() - (3.0 * 3f64.ln() + 2.0 * 2f64.ln()) / n;
        assert!((p.entropy() - expected_entropy).abs() < 1e-12);
        let expected_gini = 1.0 - (9.0 + 4.0 + 1.0) / 36.0;
        assert!((p.gini_impurity() - expected_gini).abs() < 1e-12);
    }
}
