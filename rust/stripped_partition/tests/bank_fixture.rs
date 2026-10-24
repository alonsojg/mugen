//! Checks partition-based rule validation against the Desbordante ground truth
//! in `harness/fixtures/bank.json`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use stripped_partition::{StrippedPartition, ValueId};

struct Table {
    columns: Vec<String>,
    partitions: Vec<StrippedPartition>,
}

impl Table {
    fn load(repo_root: &Path, file: &str, sep: u8) -> Self {
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(sep)
            .from_path(repo_root.join(file))
            .expect("dataset csv must exist");
        let columns: Vec<String> = reader
            .headers()
            .expect("csv header")
            .iter()
            .map(str::to_owned)
            .collect();

        let mut values: Vec<Vec<ValueId>> = vec![Vec::new(); columns.len()];
        let mut dictionaries: Vec<HashMap<String, ValueId>> = vec![HashMap::new(); columns.len()];
        for record in reader.records() {
            let record = record.expect("csv record");
            for (c, field) in record.iter().enumerate() {
                let dict = &mut dictionaries[c];
                let next = dict.len() as ValueId;
                let id = *dict.entry(field.to_owned()).or_insert(next);
                values[c].push(id);
            }
        }

        let partitions = values
            .iter()
            .map(|v| StrippedPartition::from_values(v))
            .collect();
        Self {
            columns,
            partitions,
        }
    }

    fn index(&self, name: &str) -> usize {
        self.columns
            .iter()
            .position(|c| c == name)
            .unwrap_or_else(|| panic!("no column {name}"))
    }

    fn partition_of(&self, names: &[String]) -> StrippedPartition {
        let mut iter = names.iter().map(|n| &self.partitions[self.index(n)]);
        let first = iter
            .next()
            .expect("attribute set must be non-empty")
            .clone();
        iter.fold(first, |acc, p| acc.intersect(p))
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn load_fixture() -> serde_json::Value {
    let text = std::fs::read_to_string(repo_root().join("harness/fixtures/bank.json"))
        .expect("bank.json fixture must exist");
    serde_json::from_str(&text).expect("valid json")
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .expect("array of column names")
        .iter()
        .map(|v| v.as_str().expect("column name").to_owned())
        .collect()
}

fn load_dataset(fixture: &serde_json::Value) -> Table {
    let sep = fixture["dataset"]["separator"].as_str().unwrap().as_bytes()[0];
    Table::load(
        &repo_root(),
        fixture["dataset"]["file"].as_str().unwrap(),
        sep,
    )
}

#[test]
fn bank_shape_matches_fixture() {
    let fixture = load_fixture();
    let table = load_dataset(&fixture);

    assert_eq!(
        table.columns.len() as u64,
        fixture["dataset"]["num_columns"].as_u64().unwrap()
    );
    assert_eq!(
        table.partitions[0].relation_size() as u64,
        fixture["dataset"]["num_rows"].as_u64().unwrap()
    );
}

#[test]
fn every_fixture_fd_holds_and_is_minimal() {
    let fixture = load_fixture();
    let table = load_dataset(&fixture);

    let fds = fixture["rules"]["fd"]["items"].as_array().unwrap();
    assert_eq!(
        fds.len() as u64,
        fixture["rules"]["fd"]["count"].as_u64().unwrap()
    );

    for fd in fds {
        let lhs = strings(&fd["lhs"]);
        let rhs = fd["rhs"].as_str().unwrap().to_owned();
        let lhs_partition = table.partition_of(&lhs);
        let rhs_partition = &table.partitions[table.index(&rhs)];

        assert!(
            lhs_partition.fd_holds(rhs_partition),
            "fixture FD does not hold: {lhs:?} -> {rhs}"
        );

        for i in 0..lhs.len() {
            let mut reduced = lhs.clone();
            reduced.remove(i);
            if reduced.is_empty() {
                continue;
            }
            let reduced_partition = table.partition_of(&reduced);
            assert!(
                !reduced_partition.fd_holds(rhs_partition),
                "FD not minimal, {reduced:?} -> {rhs} also holds"
            );
        }
    }
}

#[test]
fn every_fixture_ucc_is_unique_and_minimal() {
    let fixture = load_fixture();
    let table = load_dataset(&fixture);

    let uccs = fixture["rules"]["ucc"]["items"].as_array().unwrap();
    assert_eq!(
        uccs.len() as u64,
        fixture["rules"]["ucc"]["count"].as_u64().unwrap()
    );

    for ucc in uccs {
        let columns = strings(&ucc["columns"]);
        let partition = table.partition_of(&columns);
        assert_eq!(
            partition.num_non_singleton_clusters(),
            0,
            "not unique: {columns:?}"
        );

        for i in 0..columns.len() {
            let mut reduced = columns.clone();
            reduced.remove(i);
            if reduced.is_empty() {
                continue;
            }
            assert!(
                table.partition_of(&reduced).num_non_singleton_clusters() > 0,
                "UCC not minimal, {reduced:?} is already unique"
            );
        }
    }
}
