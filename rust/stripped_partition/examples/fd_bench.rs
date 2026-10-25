//! Times `discover_fds_tane` or `discover_fds_pyro` on the UCI bank datasets.
//!
//! Usage: `cargo run --release --example fd_bench -- <repo_root> <rounds> [tane|pyro] [dataset-substring]`

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use stripped_partition::{discover_fds_pyro, discover_fds_tane, Fd, StrippedPartition, ValueId};

const DATASETS: &[&str] = &[
    "bank+marketing/bank/bank.csv",
    "bank+marketing/bank/bank-full.csv",
    "bank+marketing/bank-additional/bank-additional/bank-additional.csv",
    "bank+marketing/bank-additional/bank-additional/bank-additional-full.csv",
    "cargo+2000+freight+tracking+and+tracing/c2k_data_comma.csv",
];

/// Picks `,` or `;`, whichever splits the header into more fields.
fn detect_delimiter(path: &PathBuf) -> u8 {
    let text = std::fs::read_to_string(path).expect("dataset csv must exist");
    let header = text.lines().next().unwrap_or_default();
    if header.matches(',').count() > header.matches(';').count() {
        b','
    } else {
        b';'
    }
}

fn load(path: &PathBuf) -> Vec<StrippedPartition> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(detect_delimiter(path))
        .from_path(path)
        .expect("dataset csv must exist");
    let num_columns = reader.headers().expect("csv header").len();
    let mut values: Vec<Vec<ValueId>> = vec![Vec::new(); num_columns];
    let mut dictionaries: Vec<HashMap<String, ValueId>> = vec![HashMap::new(); num_columns];
    for record in reader.records() {
        let record = record.expect("csv record");
        for (c, field) in record.iter().enumerate() {
            let dict = &mut dictionaries[c];
            let next = dict.len() as ValueId;
            values[c].push(*dict.entry(field.to_owned()).or_insert(next));
        }
    }
    values
        .iter()
        .map(|v| StrippedPartition::from_values(v))
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(&args[1]);
    let rounds: usize = args[2].parse().expect("rounds");
    let algorithm = args.get(3).map_or("tane", String::as_str);
    let discover: fn(&[StrippedPartition]) -> Vec<Fd> = match algorithm {
        "tane" => |c| discover_fds_tane(c),
        "pyro" => |c| discover_fds_pyro(c, 0.0),
        other => panic!("unknown algorithm {other}"),
    };

    let only = args.get(4).map(String::as_str);
    for name in DATASETS.iter().filter(|n| only.is_none_or(|o| n.contains(o))) {
        let path = root.join(name);
        let mut best_load = f64::INFINITY;
        let mut best_discover = f64::INFINITY;
        let mut count = 0;
        for _ in 0..rounds {
            let start = Instant::now();
            let columns = load(&path);
            best_load = best_load.min(start.elapsed().as_secs_f64());

            let start = Instant::now();
            let fds = discover(&columns);
            best_discover = best_discover.min(start.elapsed().as_secs_f64());
            count = fds.len();
        }
        println!(
            "{name}\tfds={count}\tload={best_load:.4}\tdiscover={best_discover:.4}\ttotal={:.4}",
            best_load + best_discover
        );
    }
}
