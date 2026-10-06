//! Per-address balance aggregation, either in memory or spilled to disk.
//!
//! The disk mode hash-partitions `(address, amount)` records into bucket
//! files, so that each bucket holds a disjoint set of addresses small enough
//! to aggregate in memory. Each bucket is then aggregated and sorted into a
//! run file, and the runs are k-way merged into the final order.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::fs::{self, File};
use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Balance {
    pub amount: u64,
    pub utxos: u64,
}

/// Output order: amount descending, then address ascending.
fn output_order(a: (&str, &Balance), b: (&str, &Balance)) -> Ordering {
    b.1.amount.cmp(&a.1.amount).then_with(|| a.0.cmp(b.0))
}

/// Parses a `dump` line into `(destination, amount)`. Empty lines yield `None`.
pub fn parse_dump_line(line: &str) -> Result<Option<(&str, u64)>> {
    if line.is_empty() {
        return Ok(None);
    }
    let fields: Vec<&str> = line.split(';').collect();
    let [_, _, _, _, amount, address] = fields[..] else {
        bail!("expected 6 fields, got {}", fields.len());
    };
    let amount = amount
        .parse()
        .with_context(|| format!("bad amount {amount:?}"))?;
    Ok(Some((address, amount)))
}

pub struct Aggregator {
    skip_zero: bool,
    store: Store,
}

enum Store {
    Memory(HashMap<Box<str>, Balance>),
    Disk(Spill),
}

impl Aggregator {
    pub fn in_memory(skip_zero: bool) -> Self {
        Self {
            skip_zero,
            store: Store::Memory(HashMap::new()),
        }
    }

    /// Spills to `partitions` temporary files in a fresh directory under
    /// `parent`, removed when done.
    pub fn on_disk(parent: &Path, partitions: usize, skip_zero: bool) -> Result<Self> {
        Ok(Self {
            skip_zero,
            store: Store::Disk(Spill::new(parent, partitions)?),
        })
    }

    pub fn add(&mut self, address: &str, amount: u64) -> Result<()> {
        if self.skip_zero && amount == 0 {
            return Ok(());
        }
        match &mut self.store {
            Store::Memory(m) => add_to(m, address, Balance { amount, utxos: 1 }),
            Store::Disk(s) => s.add(address, amount),
        }
    }

    /// Calls `f` for every balance of at least `minimum`, in output order.
    pub fn finish(self, minimum: u64, mut f: impl FnMut(&str, Balance) -> Result<()>) -> Result<()> {
        match self.store {
            Store::Memory(m) => {
                for (address, b) in sorted(m, minimum) {
                    f(&address, b)?;
                }
                Ok(())
            }
            Store::Disk(s) => s.finish(minimum, f),
        }
    }
}

fn add_to(m: &mut HashMap<Box<str>, Balance>, address: &str, add: Balance) -> Result<()> {
    let b = match m.get_mut(address) {
        Some(b) => b,
        None => m.entry(address.into()).or_default(),
    };
    b.amount = b.amount.checked_add(add.amount).context("amount overflow")?;
    b.utxos += add.utxos;
    Ok(())
}

fn sorted(m: HashMap<Box<str>, Balance>, minimum: u64) -> Vec<(Box<str>, Balance)> {
    let mut v: Vec<_> = m.into_iter().filter(|(_, b)| b.amount >= minimum).collect();
    v.sort_unstable_by(|a, b| output_order((&a.0, &a.1), (&b.0, &b.1)));
    v
}

struct Spill {
    dir: tempfile::TempDir,
    buckets: Vec<BufWriter<File>>,
}

impl Spill {
    fn new(parent: &Path, partitions: usize) -> Result<Self> {
        if partitions == 0 {
            bail!("partitions must be at least 1");
        }
        let dir = tempfile::Builder::new()
            .prefix("rs-chainstate-")
            .tempdir_in(parent)
            .with_context(|| format!("creating temp dir in {}", parent.display()))?;
        let buckets = (0..partitions)
            .map(|i| Ok(BufWriter::new(File::create(bucket_path(dir.path(), i))?)))
            .collect::<Result<_>>()?;
        Ok(Self { dir, buckets })
    }

    fn add(&mut self, address: &str, amount: u64) -> Result<()> {
        // DefaultHasher::default() uses fixed keys: stable within a run.
        let h = BuildHasherDefault::<DefaultHasher>::default().hash_one(address);
        let i = (h % self.buckets.len() as u64) as usize;
        write_record(&mut self.buckets[i], address, Balance { amount, utxos: 1 })
    }

    fn finish(self, minimum: u64, f: impl FnMut(&str, Balance) -> Result<()>) -> Result<()> {
        let Spill { dir, mut buckets } = self;
        for w in &mut buckets {
            w.flush()?;
        }
        let partitions = buckets.len();
        drop(buckets);

        let mut runs = Vec::with_capacity(partitions);
        for i in 0..partitions {
            let bucket = bucket_path(dir.path(), i);
            let mut m = HashMap::new();
            let mut r = BufReader::new(File::open(&bucket)?);
            while let Some((address, b)) = read_record(&mut r)? {
                add_to(&mut m, &address, b)?;
            }
            fs::remove_file(&bucket)?;

            let run = dir.path().join(format!("run-{i}"));
            let mut w = BufWriter::new(File::create(&run)?);
            for (address, b) in sorted(m, minimum) {
                write_record(&mut w, &address, b)?;
            }
            w.flush()?;
            runs.push(run);
        }
        merge(&runs, f)
        // `dir` is dropped here, removing the run files.
    }
}

fn bucket_path(dir: &Path, i: usize) -> PathBuf {
    dir.join(format!("bucket-{i}"))
}

/// Record: u32 address length, address, u64 amount, u64 utxo count (LE).
fn write_record(w: &mut impl Write, address: &str, b: Balance) -> Result<()> {
    let len = u32::try_from(address.len()).context("address too long")?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(address.as_bytes())?;
    w.write_all(&b.amount.to_le_bytes())?;
    w.write_all(&b.utxos.to_le_bytes())?;
    Ok(())
}

fn read_record(r: &mut impl Read) -> Result<Option<(String, Balance)>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let mut address = vec![0u8; u32::from_le_bytes(len) as usize];
    r.read_exact(&mut address)?;
    let mut n = [0u8; 8];
    r.read_exact(&mut n)?;
    let amount = u64::from_le_bytes(n);
    r.read_exact(&mut n)?;
    let utxos = u64::from_le_bytes(n);
    Ok(Some((String::from_utf8(address)?, Balance { amount, utxos })))
}

struct Head {
    address: String,
    balance: Balance,
    run: usize,
}

impl Ord for Head {
    // BinaryHeap is a max-heap: the entry that comes first in output order
    // must compare greatest.
    fn cmp(&self, other: &Self) -> Ordering {
        output_order(
            (&other.address, &other.balance),
            (&self.address, &self.balance),
        )
    }
}

impl PartialOrd for Head {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Head {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Head {}

/// K-way merges sorted runs. Partitions hold disjoint addresses, so no
/// address appears in two runs.
fn merge(runs: &[PathBuf], mut f: impl FnMut(&str, Balance) -> Result<()>) -> Result<()> {
    let mut readers = runs
        .iter()
        .map(|p| Ok(BufReader::new(File::open(p)?)))
        .collect::<Result<Vec<_>>>()?;
    let mut heap = BinaryHeap::with_capacity(readers.len());
    for (run, r) in readers.iter_mut().enumerate() {
        if let Some((address, balance)) = read_record(r)? {
            heap.push(Head { address, balance, run });
        }
    }
    while let Some(Head { address, balance, run }) = heap.pop() {
        f(&address, balance)?;
        if let Some((address, balance)) = read_record(&mut readers[run])? {
            heap.push(Head { address, balance, run });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "\
aa;0;1;1;100;addrA
bb;1;2;0;50;addrB
cc;0;3;0;25;addrA

dd;2;4;0;200;6a00
ee;0;5;0;0;addrC
ff;0;6;0;50;addrD
";

    fn run(mut agg: Aggregator, minimum: u64) -> Vec<(String, Balance)> {
        for line in DUMP.lines() {
            if let Some((a, amount)) = parse_dump_line(line).unwrap() {
                agg.add(a, amount).unwrap();
            }
        }
        let mut out = Vec::new();
        agg.finish(minimum, |a, b| {
            out.push((a.to_owned(), b));
            Ok(())
        })
        .unwrap();
        out
    }

    fn bal(amount: u64, utxos: u64) -> Balance {
        Balance { amount, utxos }
    }

    #[test]
    fn aggregates_and_sorts() {
        let r = run(Aggregator::in_memory(false), 0);
        assert_eq!(
            r,
            vec![
                ("6a00".into(), bal(200, 1)),
                ("addrA".into(), bal(125, 2)),
                ("addrB".into(), bal(50, 1)),
                ("addrD".into(), bal(50, 1)),
                ("addrC".into(), bal(0, 1)),
            ]
        );
    }

    #[test]
    fn skip_zero_and_minimum() {
        let r = run(Aggregator::in_memory(true), 50);
        let names: Vec<_> = r.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(names, ["6a00", "addrA", "addrB", "addrD"]);
    }

    #[test]
    fn disk_matches_memory() {
        let tmp = tempfile::tempdir().unwrap();
        for partitions in [1, 2, 7] {
            for (skip_zero, minimum) in [(false, 0), (true, 50), (false, 126)] {
                let disk = Aggregator::on_disk(tmp.path(), partitions, skip_zero).unwrap();
                assert_eq!(
                    run(disk, minimum),
                    run(Aggregator::in_memory(skip_zero), minimum),
                    "partitions={partitions} skip_zero={skip_zero} minimum={minimum}"
                );
            }
        }
        // Temp directories are cleaned up.
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse_dump_line("a;b;c").is_err());
        assert!(parse_dump_line("a;0;1;0;x;addr").is_err());
    }
}
