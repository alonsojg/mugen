//! Stripped partitions (Desbordante's `model::PositionListIndex`) in Rust.
//!
//! A stripped partition groups row ids by equal value and drops singleton
//! groups. Clusters are stored in CSR form: `rows` holds every cluster's row
//! ids back to back, and `offsets` holds the start of each cluster. Clusters
//! are ordered by their first row id and each cluster is ascending.

mod partition;

pub use partition::{RowId, StrippedPartition, ValueId, SINGLETON};
