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

use arrow::array::{Array, AsArray, GenericStringArray, OffsetSizeTrait, RecordBatch};
use arrow::compute::cast;
use arrow::datatypes::{DataType, Int32Type, Int64Type};
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
///
/// Integer and string columns are read straight from their Arrow buffers.
/// Other types are cast to strings first. A column has one Arrow type, so
/// only one of the two maps is used for a given column.
#[derive(Default)]
struct Encoder {
    strings: FxHashMap<Box<str>, ValueId>,
    ints: FxHashMap<i64, ValueId>,
    null_code: Option<ValueId>,
    next: ValueId,
}

impl Encoder {
    fn fresh(&mut self) -> ValueId {
        let c = self.next;
        self.next += 1;
        c
    }

    fn null(&mut self) -> ValueId {
        match self.null_code {
            Some(c) => c,
            None => {
                let c = self.fresh();
                self.null_code = Some(c);
                c
            }
        }
    }

    fn encode_into(&mut self, array: &dyn Array, out: &mut Vec<ValueId>) -> Result<(), ArrowError> {
        out.reserve(array.len());
        match array.data_type() {
            DataType::Int64 => {
                let a = array.as_primitive::<Int64Type>();
                self.encode_ints(a.iter(), out);
            }
            DataType::Int32 => {
                let a = array.as_primitive::<Int32Type>();
                self.encode_ints(a.iter().map(|v| v.map(i64::from)), out);
            }
            DataType::Utf8 => self.encode_strings(array.as_string::<i32>(), out),
            DataType::LargeUtf8 => self.encode_strings(array.as_string::<i64>(), out),
            _ => {
                let strings = cast(array, &DataType::Utf8)?;
                self.encode_strings(strings.as_string::<i32>(), out);
            }
        }
        Ok(())
    }

    fn encode_ints<I>(&mut self, values: I, out: &mut Vec<ValueId>)
    where
        I: Iterator<Item = Option<i64>>,
    {
        for v in values {
            let code = match v {
                None => self.null(),
                Some(x) => match self.ints.get(&x) {
                    Some(&c) => c,
                    None => {
                        let c = self.fresh();
                        self.ints.insert(x, c);
                        c
                    }
                },
            };
            out.push(code);
        }
    }

    fn encode_strings<O: OffsetSizeTrait>(
        &mut self,
        strings: &GenericStringArray<O>,
        out: &mut Vec<ValueId>,
    ) {
        for i in 0..strings.len() {
            let code = if strings.is_null(i) {
                self.null()
            } else {
                let s = strings.value(i);
                match self.strings.get(s) {
                    Some(&c) => c,
                    None => {
                        let c = self.fresh();
                        self.strings.insert(Box::from(s), c);
                        c
                    }
                }
            };
            out.push(code);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Int64Array, StringArray};
    use std::sync::Arc;

    fn batch(cols: Vec<(&str, Arc<dyn Array>)>) -> RecordBatch {
        RecordBatch::try_from_iter(cols).unwrap()
    }

    fn encode_one(array: &dyn Array) -> Vec<ValueId> {
        let mut out = Vec::new();
        Encoder::default().encode_into(array, &mut out).unwrap();
        out
    }

    fn sorted_fds(fds: Vec<Fd>) -> Vec<(Vec<usize>, usize)> {
        let mut out: Vec<_> = fds
            .into_iter()
            .map(|fd| (fd.lhs.columns().collect(), fd.rhs))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn codes_are_dense_in_first_seen_order() {
        let arr = StringArray::from(vec![Some("a"), Some("b"), Some("a"), Some("c")]);
        assert_eq!(encode_one(&arr), vec![0, 1, 0, 2]);
    }

    #[test]
    fn nulls_share_one_code_distinct_from_values() {
        let arr = StringArray::from(vec![None, Some("x"), None, Some("")]);
        // The empty string is a value, not a null.
        assert_eq!(encode_one(&arr), vec![0, 1, 0, 2]);
    }

    #[test]
    fn integer_column_encodes_like_its_string_form() {
        let ints = Int64Array::from(vec![Some(1), Some(2), Some(1), None]);
        let strs = StringArray::from(vec![Some("1"), Some("2"), Some("1"), None]);
        assert_eq!(encode_one(&ints), encode_one(&strs));
    }

    #[test]
    fn codes_are_shared_across_batches() {
        let first: Arc<dyn Array> = Arc::new(StringArray::from(vec![Some("a"), Some("b")]));
        let second: Arc<dyn Array> = Arc::new(StringArray::from(vec![Some("b"), None, Some("a")]));
        let batches = vec![
            Ok(batch(vec![("c", first)])),
            Ok(batch(vec![("c", second)])),
        ];
        let (names, codes) = encode_batches(batches).unwrap();
        assert_eq!(names, vec!["c".to_string()]);
        assert_eq!(codes, vec![vec![0, 1, 1, 2, 0]]);
    }

    #[test]
    fn column_names_come_from_the_schema() {
        let a: Arc<dyn Array> = Arc::new(Int64Array::from(vec![1, 2]));
        let b: Arc<dyn Array> = Arc::new(StringArray::from(vec!["x", "y"]));
        let batches = vec![Ok(batch(vec![("left", a), ("right", b)]))];
        let (names, codes) = encode_batches(batches).unwrap();
        assert_eq!(names, vec!["left".to_string(), "right".to_string()]);
        assert_eq!(codes.len(), 2);
    }

    #[test]
    fn empty_stream_gives_no_columns() {
        let (names, codes) = encode_batches(Vec::<Result<RecordBatch, ArrowError>>::new()).unwrap();
        assert!(names.is_empty());
        assert!(codes.is_empty());
    }

    fn partitions(cols: &[Vec<ValueId>]) -> Vec<StrippedPartition> {
        cols.iter()
            .map(|c| StrippedPartition::from_values(c))
            .collect()
    }

    #[test]
    fn finds_single_column_fds_both_ways() {
        // a and b determine each other; c is unrelated.
        let cols = vec![vec![0, 0, 1, 1], vec![0, 0, 1, 1], vec![0, 1, 0, 1]];
        let fds = run_pyro(&partitions(&cols), 0.0, None, None).unwrap();
        let got = sorted_fds(fds);
        assert!(got.contains(&(vec![0], 1)));
        assert!(got.contains(&(vec![1], 0)));
    }

    #[test]
    fn max_lhs_hides_fds_with_larger_left_hand_side() {
        // (a, b) -> c holds, but neither a nor b alone does.
        let cols = vec![vec![0, 0, 1, 1], vec![0, 1, 0, 1], vec![0, 1, 2, 3]];
        let full = sorted_fds(run_pyro(&partitions(&cols), 0.0, None, None).unwrap());
        assert!(full.contains(&(vec![0, 1], 2)));
        let capped = sorted_fds(run_pyro(&partitions(&cols), 0.0, Some(1), None).unwrap());
        assert!(!capped.iter().any(|(lhs, _)| lhs.len() > 1));
    }

    #[test]
    fn thread_count_does_not_change_the_result() {
        let cols = vec![
            vec![0, 0, 1, 1, 2, 2],
            vec![0, 1, 0, 1, 0, 1],
            vec![0, 1, 2, 3, 4, 5],
            vec![0, 0, 0, 1, 1, 1],
        ];
        let serial = sorted_fds(run_pyro(&partitions(&cols), 0.0, None, Some(1)).unwrap());
        let parallel = sorted_fds(run_pyro(&partitions(&cols), 0.0, None, Some(4)).unwrap());
        assert_eq!(serial, parallel);
    }
}
