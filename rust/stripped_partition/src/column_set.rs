use fixedbitset::FixedBitSet;

/// A set of column indices stored as a dynamic bitset.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ColumnSet {
    bits: FixedBitSet,
}

impl ColumnSet {
    pub fn empty(num_columns: usize) -> Self {
        Self {
            bits: FixedBitSet::with_capacity(num_columns),
        }
    }

    pub fn singleton(num_columns: usize, column: usize) -> Self {
        Self::empty(num_columns).with(column)
    }

    pub fn from_columns(num_columns: usize, columns: impl IntoIterator<Item = usize>) -> Self {
        let mut set = Self::empty(num_columns);
        for c in columns {
            set.bits.insert(c);
        }
        set
    }

    pub fn with(&self, column: usize) -> Self {
        let mut bits = self.bits.clone();
        bits.insert(column);
        Self { bits }
    }

    pub fn without(&self, column: usize) -> Self {
        let mut bits = self.bits.clone();
        bits.set(column, false);
        Self { bits }
    }

    pub fn intersection(&self, other: &Self) -> Self {
        let mut bits = self.bits.clone();
        bits.intersect_with(&other.bits);
        Self { bits }
    }

    pub fn contains(&self, column: usize) -> bool {
        self.bits.contains(column)
    }

    pub fn len(&self) -> usize {
        self.bits.count_ones(..)
    }

    pub fn is_empty(&self) -> bool {
        self.bits.is_clear()
    }

    pub fn max(&self) -> Option<usize> {
        self.bits.ones().next_back()
    }

    pub fn is_subset_of(&self, other: &Self) -> bool {
        self.bits.is_subset(&other.bits)
    }

    pub fn columns(&self) -> impl Iterator<Item = usize> + '_ {
        self.bits.ones()
    }
}
