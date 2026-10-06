# rs-chainstate

Bitcoin Core chainstate (UTXO set) parser, in Rust. Rewrite of
[mycroft/chainstate](https://github.com/mycroft/chainstate).

## Safety

Opening a LevelDB database writes to it, even if you only read from it: it
replays the log, rewrites the MANIFEST and deletes old files. Never open
bitcoind's own `chainstate/` directory. Work on a copy:

```sh
# Stop bitcoind first to get a consistent copy.
rs-chainstate snapshot /data/bitcoin/data/chainstate ./cs
rs-chainstate info ./cs
rs-chainstate dump ./cs > utxos.csv
```

`snapshot` copies the files without opening them as a database, and adds a
marker file to the copy. The other commands refuse to open a directory that
has no marker (unless you pass `--force`), or whose `LOCK` file is held by
another process through an fcntl lock (as bitcoind's is).

## Output

`dump` prints `txid;vout;height;coinbase;amount_sats;destination`. The
destination is an address, or a tag for scripts without one:

- `p2pk:<pubkey>`: pay-to-pubkey (mostly 2009-2010 coinbases). With
  `--p2pk-as-p2pkh`, shown as the P2PKH address of the key instead, as block
  explorers do.
- `multisig:<script>`: bare multisig.
- `nonstandard:<script>`: anything else (e.g. early P2Pool share commitments).

`balances [FILE|-]` reads a dump and prints `address;amount_sats;utxo_count`,
sorted by amount (largest first), with no header unless you pass `--header`.
`--skip-zero` ignores 0-value UTXOs, and `--minimum <sats>` keeps only
balances of at least that amount:

```sh
rs-chainstate dump --p2pk-as-p2pkh ./cs | rs-chainstate balances --skip-zero --header --minimum 100000000 > balances.csv
```

`dump-balances` does both in one pass, without writing the intermediate dump.
It takes the options of both commands, plus `-o FILE`:

```sh
rs-chainstate dump-balances --p2pk-as-p2pkh --skip-zero --header \
  --minimum 100000000 -o balances.csv ./cs
```

By default, balances are aggregated in memory. If they don't fit, pass
`--temp-dir DIR` to spill them to disk: records are split by address hash
into `--partitions` files (256 by default), each file is aggregated and
sorted on its own, and the results are merged. The output is identical in
both modes. The temporary directory is removed at the end. It needs about
54 bytes of disk per UTXO plus the address length.

## Benchmark

Full mainnet chainstate (15 GB, 165,053,673 UTXOs, 20,093,938 BTC), best block
`000000000000000000000038a4e9b64bdd62533567b14ceb6f3176bb12fcceee`, October
2026. Machine: Intel i7-13700K, 62 GB RAM, chainstate snapshot on a btrfs HDD
(Seagate ST4000DM004). Release build:

```sh
rs-chainstate dump-balances --minimum 1000 -o balances.csv ./cs
```

| Mode      | Wall time        | User   | Sys  | Peak RSS | Output lines |
|-----------|------------------|--------|------|----------|--------------|
| in memory | 346.5 s (5m47s) | 287.5 s | 9.4 s | 10.8 GB | 51,801,656 |

`snapshot` took 0.17 s: on btrfs, the copy shares the file's blocks instead
of duplicating the data.
