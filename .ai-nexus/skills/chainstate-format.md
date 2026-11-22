---
name: chainstate-format
description: Decoding Bitcoin Core's chainstate and the dump output
applies_to: ["src/**/*.rs"]
---
rs-chainstate reads Bitcoin Core's UTXO set from a LevelDB copy and prints
one line per coin, or balances per address. A decoding slip silently
corrupts every line, so review the change against Bitcoin Core's format:

- Values are XORed with the obfuscation key, repeated over the value's
  length; the key itself is stored under `\x0e\x00obfuscate_key`.
- Keys are `C`, the txid (stored little-endian, printed reversed), then the
  output index as a varint.
- Bitcoin Core's VARINT: base-128, most significant group first, with one
  added to each group before the last; not LEB128, and overflow past 64
  bits must be an error.
- The coin header is `height * 2 + coinbase`; amounts use CompressAmount,
  so decompression must handle zero and the exponent and mantissa cases.
- Scripts are compressed: size 0 P2PKH, 1 P2SH, 2 and 3 compressed P2PK,
  4 and 5 P2PK whose uncompressed key must be rebuilt, 6 and above a raw
  script of `size - 6` bytes.
- Destinations: the address for the configured network, `p2pk:`,
  `multisig:` and `nonstandard:` tags as the README documents; sums of
  amounts that can overflow; the `txid;vout;height;coinbase;amount_sats;
  destination` and balances formats unchanged unless the README changes too.
- Panics on truncated or unexpected values instead of an error naming the key.

Do not report formatting or Clippy lints: CI runs both. Only report what is
visible in the diff and the files given.
