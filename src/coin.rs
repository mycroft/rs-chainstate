//! Decoding of chainstate records (`txdb.cpp`, `coins.h`, `compressor.cpp`).

use anyhow::{Context, Result, bail};
use bitcoin::hashes::Hash;
use bitcoin::secp256k1::PublicKey;
use bitcoin::{BlockHash, OutPoint, ScriptBuf, Txid};

use crate::varint::{decompress_amount, read_varint};

/// Key prefix of a coin record: 'C' || txid || VARINT(vout).
pub const DB_COIN: u8 = b'C';
/// Key of the best block hash.
pub const DB_BEST_BLOCK: &[u8] = b"B";
/// Key of the obfuscation key: "\x0e\x00obfuscate_key".
pub const OBFUSCATE_KEY_KEY: &[u8] = b"\x0e\x00obfuscate_key";

#[derive(Debug, Clone)]
pub struct Coin {
    pub outpoint: OutPoint,
    pub height: u32,
    pub coinbase: bool,
    pub amount: u64,
    pub script: ScriptBuf,
}

/// XORs `data` in place with the repeating obfuscation key.
pub fn deobfuscate(data: &mut [u8], key: &[u8]) {
    if key.is_empty() {
        return;
    }
    for (b, k) in data.iter_mut().zip(key.iter().cycle()) {
        *b ^= k;
    }
}

/// The obfuscation key value is itself serialized as a vector: CompactSize length + bytes.
pub fn parse_obfuscation_key(raw: &[u8]) -> Result<Vec<u8>> {
    let (&len, rest) = raw.split_first().context("empty obfuscation key")?;
    if rest.len() != len as usize {
        bail!("bad obfuscation key length: {} vs {}", len, rest.len());
    }
    Ok(rest.to_vec())
}

pub fn parse_best_block(value: &[u8]) -> Result<BlockHash> {
    BlockHash::from_slice(value).context("bad best block hash")
}

pub fn parse_outpoint(key: &[u8]) -> Result<OutPoint> {
    if key.len() < 34 || key[0] != DB_COIN {
        bail!("not a coin key");
    }
    let txid = Txid::from_slice(&key[1..33])?;
    let mut rest = &key[33..];
    let vout = u32::try_from(read_varint(&mut rest)?).context("vout overflow")?;
    Ok(OutPoint { txid, vout })
}

/// Decodes an already deobfuscated coin value.
pub fn parse_coin(outpoint: OutPoint, mut value: &[u8]) -> Result<Coin> {
    let code = read_varint(&mut value)?;
    let amount = decompress_amount(read_varint(&mut value)?);
    let script = decompress_script(&mut value)?;
    Ok(Coin {
        outpoint,
        height: u32::try_from(code >> 1).context("height overflow")?,
        coinbase: code & 1 == 1,
        amount,
        script,
    })
}

fn take<'a>(buf: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if buf.len() < n {
        bail!("truncated script: want {n} bytes, have {}", buf.len());
    }
    let (head, tail) = buf.split_at(n);
    *buf = tail;
    Ok(head)
}

/// Inverse of Bitcoin Core's `ScriptCompression`.
fn decompress_script(buf: &mut &[u8]) -> Result<ScriptBuf> {
    use bitcoin::opcodes::all::*;
    use bitcoin::script::Builder;

    let size = read_varint(buf)?;
    let script = match size {
        0 => {
            let h: [u8; 20] = take(buf, 20)?.try_into()?;
            Builder::new()
                .push_opcode(OP_DUP)
                .push_opcode(OP_HASH160)
                .push_slice(h)
                .push_opcode(OP_EQUALVERIFY)
                .push_opcode(OP_CHECKSIG)
                .into_script()
        }
        1 => {
            let h: [u8; 20] = take(buf, 20)?.try_into()?;
            Builder::new()
                .push_opcode(OP_HASH160)
                .push_slice(h)
                .push_opcode(OP_EQUAL)
                .into_script()
        }
        2..=5 => {
            let mut pk = [0u8; 33];
            pk[0] = size as u8;
            pk[1..].copy_from_slice(take(buf, 32)?);
            let pk_bytes: Vec<u8> = if size <= 3 {
                pk.to_vec()
            } else {
                pk[0] -= 2;
                PublicKey::from_slice(&pk)
                    .context("invalid P2PK public key")?
                    .serialize_uncompressed()
                    .to_vec()
            };
            let push = bitcoin::script::PushBytesBuf::try_from(pk_bytes)?;
            Builder::new()
                .push_slice(push)
                .push_opcode(OP_CHECKSIG)
                .into_script()
        }
        n => {
            let len = usize::try_from(n - 6)?;
            ScriptBuf::from_bytes(take(buf, len)?.to_vec())
        }
    };
    Ok(script)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deobfuscate_cycles_key() {
        let mut data = [0xffu8; 5];
        deobfuscate(&mut data, &[0x0f, 0xf0]);
        assert_eq!(data, [0xf0, 0x0f, 0xf0, 0x0f, 0xf0]);
    }

    #[test]
    fn p2pkh_coin() {
        // height 1, coinbase, 50 BTC, P2PKH.
        let mut v = vec![0x03, 0x32, 0x00];
        v.extend([0xab; 20]);
        let c = parse_coin(OutPoint::null(), &v).unwrap();
        assert_eq!(c.height, 1);
        assert!(c.coinbase);
        assert_eq!(c.amount, 5_000_000_000);
        assert!(c.script.is_p2pkh());
    }

    #[test]
    fn raw_script_coin() {
        // nSize 6 + 2 => 2-byte raw script (OP_TRUE OP_TRUE).
        let v = [0x00, 0x00, 0x08, 0x51, 0x51];
        let c = parse_coin(OutPoint::null(), &v).unwrap();
        assert_eq!(c.script.as_bytes(), &[0x51, 0x51]);
    }

    #[test]
    fn outpoint_key() {
        let mut k = vec![DB_COIN];
        k.extend([0x11; 32]);
        k.extend([0x80, 0x00]); // vout 128
        let op = parse_outpoint(&k).unwrap();
        assert_eq!(op.vout, 128);
    }
}
