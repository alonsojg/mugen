use std::collections::HashSet;

use crate::column_set::ColumnSet;

/// Next level of the subset lattice: every set `T` of size `k + 1` whose
/// `k`-subsets all appear in `prev`.
///
/// Each `T` is generated once, from `T` without its largest column, so the
/// result has no duplicates and the input order is irrelevant.
pub fn next_level(prev: &[ColumnSet], num_columns: usize) -> Vec<ColumnSet> {
    let known: HashSet<&ColumnSet> = prev.iter().collect();
    let mut next = Vec::new();
    for set in prev {
        let start = set.max().map_or(0, |m| m + 1);
        for c in start..num_columns {
            let candidate = set.with(c);
            let all_subsets_known = candidate.columns().all(|x| {
                let sub = candidate.without(x);
                sub.is_empty() || known.contains(&sub)
            });
            if all_subsets_known {
                next.push(candidate);
            }
        }
    }
    next
}
