//! Python bindings for `stripped_partition`.
//!
//! The entry point takes any object that implements the Arrow PyCapsule
//! stream protocol (`__arrow_c_stream__`): `pyarrow.Table`, Polars
//! `DataFrame`, or any other Arrow-compatible table. Columns are dictionary
//! encoded into dense `u32` codes, then each column becomes a stripped
//! partition and the Pyro search runs on those.
//!
//! Null is treated as one ordinary value. All nulls in a column are equal to
//! each other and different from every non-null value.

use std::ffi::CStr;

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::compute::cast;
use arrow::datatypes::DataType;
use arrow::error::ArrowError;
use arrow::ffi_stream::{ArrowArrayStreamReader, FFI_ArrowArrayStream};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyCapsule;
use rustc_hash::FxHashMap;
use stripped_partition::{
    discover_fds_pyro_limited, Fd, StrippedPartition, ValueId, DEFAULT_CACHE_BUDGET_BYTES,
};

const STREAM_CAPSULE: &CStr = c"arrow_array_stream";

/// Dictionary for one column. Values get dense codes in first-seen order.
#[derive(Default)]
struct Encoder {
    codes: FxHashMap<Box<str>, ValueId>,
    null_code: Option<ValueId>,
    next: ValueId,
}

impl Encoder {
    fn fresh(&mut self) -> ValueId {
        let c = self.next;
        self.next += 1;
        c
    }

    fn encode_into(&mut self, array: &dyn Array, out: &mut Vec<ValueId>) -> Result<(), ArrowError> {
        let strings = cast(array, &DataType::Utf8)?;
        let strings = strings.as_string::<i32>();
        out.reserve(strings.len());
        for i in 0..strings.len() {
            let code = if strings.is_null(i) {
                match self.null_code {
                    Some(c) => c,
                    None => {
                        let c = self.fresh();
                        self.null_code = Some(c);
                        c
                    }
                }
            } else {
                let s = strings.value(i);
                match self.codes.get(s) {
                    Some(&c) => c,
                    None => {
                        let c = self.fresh();
                        self.codes.insert(Box::from(s), c);
                        c
                    }
                }
            };
            out.push(code);
        }
        Ok(())
    }
}

/// Reads every batch and returns the column names and per-column codes.
fn encode_batches<I>(batches: I) -> Result<(Vec<String>, Vec<Vec<ValueId>>), String>
where
    I: IntoIterator<Item = Result<RecordBatch, ArrowError>>,
{
    let mut names: Option<Vec<String>> = None;
    let mut codes: Vec<Vec<ValueId>> = Vec::new();
    let mut encoders: Vec<Encoder> = Vec::new();

    for batch in batches {
        let batch = batch.map_err(|e| e.to_string())?;
        if names.is_none() {
            names = Some(
                batch
                    .schema()
                    .fields()
                    .iter()
                    .map(|f| f.name().clone())
                    .collect(),
            );
            codes = vec![Vec::new(); batch.num_columns()];
            encoders = (0..batch.num_columns())
                .map(|_| Encoder::default())
                .collect();
        }
        for (i, array) in batch.columns().iter().enumerate() {
            encoders[i]
                .encode_into(array.as_ref(), &mut codes[i])
                .map_err(|e| e.to_string())?;
        }
    }
    Ok((names.unwrap_or_default(), codes))
}

/// Runs Pyro on the partitions. `threads` sets a dedicated rayon pool, and
/// `None` uses the global pool.
fn run_pyro(
    columns: &[StrippedPartition],
    max_error: f64,
    max_lhs: Option<usize>,
    threads: Option<usize>,
) -> Result<Vec<Fd>, String> {
    let max_lhs = max_lhs.unwrap_or(usize::MAX);
    let search =
        || discover_fds_pyro_limited(columns, max_error, max_lhs, DEFAULT_CACHE_BUDGET_BYTES);
    match threads {
        None => Ok(search()),
        Some(n) => {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .build()
                .map_err(|e| e.to_string())?;
            Ok(pool.install(search))
        }
    }
}

/// `(lhs_indices, rhs_index)` for each minimal FD.
type FdIndices = Vec<(Vec<usize>, usize)>;

/// Discover minimal functional dependencies in an Arrow-compatible table.
///
/// Returns `(column_names, fds)`, where each FD is `(lhs_indices, rhs_index)`
/// and indices refer to `column_names`.
#[pyfunction]
#[pyo3(signature = (data, max_error=0.0, max_lhs=None, threads=None))]
fn discover_fds(
    py: Python<'_>,
    data: &Bound<'_, PyAny>,
    max_error: f64,
    max_lhs: Option<usize>,
    threads: Option<usize>,
) -> PyResult<(Vec<String>, FdIndices)> {
    if !(0.0..1.0).contains(&max_error) {
        return Err(PyValueError::new_err("max_error must be in [0, 1)"));
    }
    if threads == Some(0) {
        return Err(PyValueError::new_err("threads must be positive"));
    }

    let capsule_obj = data.call_method0("__arrow_c_stream__")?;
    let capsule = capsule_obj.cast::<PyCapsule>()?;
    let ptr = capsule
        .pointer_checked(Some(STREAM_CAPSULE))?
        .as_ptr()
        .cast::<FFI_ArrowArrayStream>();
    // SAFETY: The capsule is named `arrow_array_stream` and holds a
    // `FFI_ArrowArrayStream` produced by the source. `from_raw` moves the
    // stream out and leaves the capsule empty, so the producer will not
    // release it a second time.
    let reader = unsafe { ArrowArrayStreamReader::from_raw(ptr) }
        .map_err(|e| PyValueError::new_err(e.to_string()))?;

    let (names, codes) = encode_batches(reader).map_err(PyValueError::new_err)?;
    let partitions: Vec<StrippedPartition> = codes
        .iter()
        .map(|c| StrippedPartition::from_values(c))
        .collect();

    let fds = py
        .detach(|| run_pyro(&partitions, max_error, max_lhs, threads))
        .map_err(PyRuntimeError::new_err)?;

    let out = fds
        .into_iter()
        .map(|fd| (fd.lhs.columns().collect(), fd.rhs))
        .collect();
    Ok((names, out))
}

#[pymodule]
fn mugen(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(discover_fds, m)?)?;
    Ok(())
}
