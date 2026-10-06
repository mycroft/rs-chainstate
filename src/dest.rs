//! Human readable destination of an output script, as printed by `dump`.

use bitcoin::hashes::Hash;
use bitcoin::{Address, Network, PubkeyHash, Script};

/// Returns the address of `script`, or a `type:hex` tag when it has none:
/// `p2pk:<pubkey>`, `multisig:<script>` or `nonstandard:<script>`.
///
/// With `p2pk_as_p2pkh`, P2PK outputs are shown as the P2PKH address of their
/// key (what explorers do): the same private key controls both.
pub fn destination(script: &Script, network: Network, p2pk_as_p2pkh: bool) -> String {
    if let Ok(a) = Address::from_script(script, network) {
        return a.to_string();
    }
    if script.is_p2pk() {
        // <push len> <pubkey> OP_CHECKSIG. Kept as raw bytes: some keys are invalid.
        let b = script.as_bytes();
        let pubkey = &b[1..b.len() - 1];
        if p2pk_as_p2pkh {
            return Address::p2pkh(PubkeyHash::hash(pubkey), network).to_string();
        }
        return format!("p2pk:{}", crate::hex(pubkey));
    }
    let kind = if script.is_multisig() {
        "multisig"
    } else {
        "nonstandard"
    };
    format!("{kind}:{}", crate::hex(script.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::ScriptBuf;

    fn script(hex: &str) -> ScriptBuf {
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        ScriptBuf::from_bytes(bytes)
    }

    // Genesis block coinbase output.
    const GENESIS_P2PK: &str = "4104678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5fac";

    #[test]
    fn p2pk() {
        let s = script(GENESIS_P2PK);
        assert_eq!(
            destination(&s, Network::Bitcoin, false),
            format!("p2pk:{}", &GENESIS_P2PK[2..GENESIS_P2PK.len() - 2])
        );
        assert_eq!(
            destination(&s, Network::Bitcoin, true),
            "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"
        );
    }

    #[test]
    fn multisig() {
        // 1-of-1 bare multisig with a compressed key.
        let hex = "512102000000000000000000000000000000000000000000000000000000000000000151ae";
        let s = script(hex);
        assert_eq!(destination(&s, Network::Bitcoin, true), format!("multisig:{hex}"));
    }

    #[test]
    fn nonstandard() {
        // Early P2Pool share commitment: bare 36-byte push.
        let hex = format!("24{}", "ab".repeat(36));
        let s = script(&hex);
        assert_eq!(destination(&s, Network::Bitcoin, true), format!("nonstandard:{hex}"));
    }
}
