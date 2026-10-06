mod balances;
mod chainstate;
mod coin;
mod dest;
mod safety;
mod varint;

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use bitcoin::Network;
use clap::{Args, Parser, Subcommand};

use crate::balances::Aggregator;
use crate::chainstate::Chainstate;
use crate::coin::Coin;

#[derive(Parser)]
#[command(version, about = "Bitcoin Core chainstate (UTXO set) parser")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Copy a chainstate directory to a disposable snapshot (never opens the source).
    Snapshot {
        src: PathBuf,
        dst: PathBuf,
        /// Copy even if bitcoind holds the lock (result may be inconsistent).
        #[arg(long)]
        allow_live: bool,
    },
    /// Show best block and obfuscation key of a snapshot.
    Info {
        dir: PathBuf,
        /// Open a directory that was not created by `snapshot`.
        #[arg(long)]
        force: bool,
    },
    /// Dump all UTXOs as `txid;vout;height;coinbase;amount;destination`.
    ///
    /// Destination is an address, or `p2pk:<pubkey>`, `multisig:<script>` or
    /// `nonstandard:<script>` for outputs without one.
    Dump(DumpArgs),
    /// Compute per-address balances from a `dump` output (`-` for stdin).
    /// Prints `address;amount;utxo_count`, sorted by amount descending.
    Balances {
        #[arg(default_value = "-")]
        input: PathBuf,
        #[command(flatten)]
        balances: BalanceArgs,
    },
    /// `dump` + `balances` in one pass, without the intermediate dump.
    DumpBalances {
        #[command(flatten)]
        dump: DumpArgs,
        #[command(flatten)]
        balances: BalanceArgs,
    },
}

#[derive(Args)]
struct DumpArgs {
    dir: PathBuf,
    /// Open a directory that was not created by `snapshot`.
    #[arg(long)]
    force: bool,
    #[arg(long, default_value = "bitcoin")]
    network: Network,
    /// Show P2PK outputs as the P2PKH address of their public key.
    #[arg(long)]
    p2pk_as_p2pkh: bool,
}

#[derive(Args)]
struct BalanceArgs {
    /// Ignore 0-value UTXOs.
    #[arg(long)]
    skip_zero: bool,
    /// Only print addresses whose balance is at least this many satoshis.
    #[arg(long, default_value_t = 0)]
    minimum: u64,
    /// Print a header line first.
    #[arg(long)]
    header: bool,
    /// Write to this file instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Aggregate on disk in a temporary directory created under DIR, instead
    /// of in memory. Needs roughly the size of a dump in free space.
    #[arg(long, value_name = "DIR")]
    temp_dir: Option<PathBuf>,
    /// Number of temporary files with --temp-dir. Memory use is about the
    /// in-memory usage divided by this.
    #[arg(long, default_value_t = 256, requires = "temp_dir")]
    partitions: usize,
}

impl BalanceArgs {
    fn aggregator(&self) -> Result<Aggregator> {
        match &self.temp_dir {
            Some(dir) => Aggregator::on_disk(dir, self.partitions, self.skip_zero),
            None => Ok(Aggregator::in_memory(self.skip_zero)),
        }
    }

    fn write(&self, agg: Aggregator) -> Result<()> {
        let out: Box<dyn Write> = match &self.output {
            Some(p) => Box::new(
                File::create(p).with_context(|| format!("creating {}", p.display()))?,
            ),
            None => Box::new(std::io::stdout().lock()),
        };
        let mut out = BufWriter::new(out);
        if self.header {
            writeln!(out, "address;amount;utxo_count")?;
        }
        agg.finish(self.minimum, |address, b| {
            writeln!(out, "{address};{};{}", b.amount, b.utxos)?;
            Ok(())
        })?;
        out.flush()?;
        Ok(())
    }
}

impl DumpArgs {
    /// Calls `f` with each decodable coin and its destination.
    fn for_each_coin(&self, mut f: impl FnMut(&Coin, &str) -> Result<()>) -> Result<()> {
        let mut cs = Chainstate::open(&self.dir, self.force)?;
        let mut errors = 0u64;
        cs.for_each_coin(|c| match c {
            Ok(c) => f(&c, &dest::destination(&c.script, self.network, self.p2pk_as_p2pkh)),
            Err(e) => {
                errors += 1;
                eprintln!("error: {e:#}");
                Ok(())
            }
        })?;
        if errors > 0 {
            eprintln!("{errors} undecodable coins");
        }
        Ok(())
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Snapshot {
            src,
            dst,
            allow_live,
        } => {
            safety::snapshot(&src, &dst, allow_live)?;
            eprintln!("snapshot written to {}", dst.display());
        }
        Command::Info { dir, force } => {
            let mut cs = Chainstate::open(&dir, force)?;
            println!("obfuscation key: {}", hex(cs.obfuscation_key()));
            match cs.best_block()? {
                Some(h) => println!("best block: {h}"),
                None => println!("best block: <none>"),
            }
        }
        Command::Dump(args) => {
            let mut out = BufWriter::new(std::io::stdout().lock());
            args.for_each_coin(|c, dest| {
                writeln!(
                    out,
                    "{};{};{};{};{};{}",
                    c.outpoint.txid,
                    c.outpoint.vout,
                    c.height,
                    u8::from(c.coinbase),
                    c.amount,
                    dest
                )?;
                Ok(())
            })?;
            out.flush()?;
        }
        Command::Balances { input, balances } => {
            let reader: Box<dyn BufRead> = if input.as_os_str() == "-" {
                Box::new(std::io::stdin().lock())
            } else {
                Box::new(BufReader::new(File::open(&input)?))
            };
            let mut agg = balances.aggregator()?;
            for (i, line) in reader.lines().enumerate() {
                let line = line?;
                let parsed =
                    balances::parse_dump_line(&line).with_context(|| format!("line {}", i + 1))?;
                if let Some((address, amount)) = parsed {
                    agg.add(address, amount)?;
                }
            }
            balances.write(agg)?;
        }
        Command::DumpBalances { dump, balances } => {
            let mut agg = balances.aggregator()?;
            dump.for_each_coin(|c, dest| agg.add(dest, c.amount))?;
            balances.write(agg)?;
        }
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
