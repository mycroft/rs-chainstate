use std::path::Path;

use anyhow::{Context, Result};
use bitcoin::BlockHash;
use rusty_leveldb::{DB, LdbIterator, Options};

use crate::coin::{self, Coin};
use crate::safety;

pub struct Chainstate {
    db: DB,
    obfuscation_key: Vec<u8>,
}

impl Chainstate {
    /// Opens a chainstate snapshot. See `safety` for why this must never be
    /// pointed at bitcoind's own directory.
    pub fn open(dir: &Path, force: bool) -> Result<Self> {
        safety::check_openable(dir, force)?;
        let opts = Options {
            create_if_missing: false,
            ..Default::default()
        };
        let mut db = DB::open(dir, opts).with_context(|| format!("opening {}", dir.display()))?;
        let obfuscation_key = match db.get(coin::OBFUSCATE_KEY_KEY) {
            Some(raw) => coin::parse_obfuscation_key(&raw)?,
            None => Vec::new(),
        };
        Ok(Self {
            db,
            obfuscation_key,
        })
    }

    pub fn obfuscation_key(&self) -> &[u8] {
        &self.obfuscation_key
    }

    fn get(&mut self, key: &[u8]) -> Option<Vec<u8>> {
        let mut v = self.db.get(key)?.to_vec();
        coin::deobfuscate(&mut v, &self.obfuscation_key);
        Some(v)
    }

    pub fn best_block(&mut self) -> Result<Option<BlockHash>> {
        self.get(coin::DB_BEST_BLOCK)
            .map(|v| coin::parse_best_block(&v))
            .transpose()
    }

    /// Iterates over all coins, calling `f` for each decoded coin or error.
    pub fn for_each_coin(&mut self, mut f: impl FnMut(Result<Coin>) -> Result<()>) -> Result<()> {
        let mut it = self.db.new_iter()?;
        it.seek(&[coin::DB_COIN]);
        while let Some((k, v)) = it.current() {
            if k.first() != Some(&coin::DB_COIN) {
                break;
            }
            let mut v = v.to_vec();
            coin::deobfuscate(&mut v, &self.obfuscation_key);
            f(coin::parse_outpoint(&k).and_then(|op| coin::parse_coin(op, &v)))?;
            if !it.advance() {
                break;
            }
        }
        Ok(())
    }
}
