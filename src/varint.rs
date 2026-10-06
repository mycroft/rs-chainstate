//! Bitcoin Core serialization primitives used by the chainstate
//! (`serialize.h` VARINT and `compressor.cpp` amount compression).

use anyhow::{Result, bail};

/// Reads a Bitcoin Core VARINT (MSB base-128, with the "+1" per continuation
/// byte trick). Not to be confused with the CompactSize used in transactions.
pub fn read_varint(buf: &mut &[u8]) -> Result<u64> {
    let mut n: u64 = 0;
    loop {
        let Some((&b, rest)) = buf.split_first() else {
            bail!("truncated varint");
        };
        *buf = rest;
        if n > (u64::MAX >> 7) {
            bail!("varint overflow");
        }
        n = (n << 7) | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            return Ok(n);
        }
        n = n.checked_add(1).ok_or_else(|| anyhow::anyhow!("varint overflow"))?;
    }
}

/// Inverse of Bitcoin Core's `CompressAmount`.
pub fn decompress_amount(mut x: u64) -> u64 {
    if x == 0 {
        return 0;
    }
    x -= 1;
    let mut e = x % 10;
    x /= 10;
    let mut n = if e < 9 {
        let d = (x % 9) + 1;
        x /= 9;
        x * 10 + d
    } else {
        x + 1
    };
    while e > 0 {
        n *= 10;
        e -= 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(bytes: &[u8]) -> u64 {
        let mut b = bytes;
        let v = read_varint(&mut b).unwrap();
        assert!(b.is_empty());
        v
    }

    #[test]
    fn varint_vectors() {
        // From Bitcoin Core serialize.h comments.
        assert_eq!(varint(&[0x00]), 0);
        assert_eq!(varint(&[0x7f]), 127);
        assert_eq!(varint(&[0x80, 0x00]), 128);
        assert_eq!(varint(&[0x80, 0x7f]), 255);
        assert_eq!(varint(&[0x82, 0xfe, 0x7f]), 65535);
        assert_eq!(varint(&[0x8e, 0xfe, 0xfe, 0xff, 0x00]), 4294967296);
    }

    #[test]
    fn varint_truncated() {
        assert!(read_varint(&mut &[0x80][..]).is_err());
    }

    #[test]
    fn amount_vectors() {
        // From Bitcoin Core compress_tests.cpp.
        const COIN: u64 = 100_000_000;
        assert_eq!(decompress_amount(0x0), 0);
        assert_eq!(decompress_amount(0x1), 1);
        assert_eq!(decompress_amount(0x7), 1_000_000);
        assert_eq!(decompress_amount(0x9), COIN);
        assert_eq!(decompress_amount(0x32), 50 * COIN);
        assert_eq!(decompress_amount(0x1406f40), 21_000_000 * COIN);
    }
}
