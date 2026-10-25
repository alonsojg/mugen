//! Stripped partitions (Desbordante's `model::PositionListIndex`) in Rust.
//!
//! A stripped partition groups row ids by equal value and drops singleton
//! groups. Clusters are stored in CSR form: `rows` holds every cluster's row
//! ids back to back, and `offsets` holds the start of each cluster. Clusters
//! are ordered by their first row id and each cluster is ascending.

mod afd;
mod column_set;
mod discovery;
mod lattice;
mod partition;
mod pyro;
mod tane;

pub use afd::{discover_afds_tane, Afd, AfdMeasure};
pub use column_set::ColumnSet;
pub use discovery::{discover_fds, discover_uccs, Fd};
pub use lattice::next_level;
pub use partition::{RowId, StrippedPartition, ValueId, SINGLETON};
pub use pyro::discover_fds_pyro;
pub use tane::discover_fds_tane;
