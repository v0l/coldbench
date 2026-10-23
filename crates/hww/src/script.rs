use bitcoin::Network;

/// The single key address types the devices here can all sign.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum Script {
    /// P2PKH, `1...`, BIP44.
    Legacy,
    /// P2SH-P2WPKH, `3...`, BIP49.
    Nested,
    /// P2WPKH, `bc1q...`, BIP84.
    Native,
}

impl Script {
    pub const ALL: [Script; 3] = [Script::Native, Script::Nested, Script::Legacy];

    pub fn label(self) -> &'static str {
        match self {
            Script::Legacy => "legacy (P2PKH)",
            Script::Nested => "nested segwit (P2SH-P2WPKH)",
            Script::Native => "native segwit (P2WPKH)",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Script::Legacy => "P2PKH",
            Script::Nested => "P2SH-P2WPKH",
            Script::Native => "P2WPKH",
        }
    }

    pub fn purpose(self) -> u32 {
        match self {
            Script::Legacy => 44,
            Script::Nested => 49,
            Script::Native => 84,
        }
    }

    /// Wrap a key expression (`[fp/84'/0'/0']xpub.../0/*`) in this script's output descriptor.
    pub fn descriptor(self, key: &str) -> String {
        match self {
            Script::Legacy => format!("pkh({key})"),
            Script::Nested => format!("sh(wpkh({key}))"),
            Script::Native => format!("wpkh({key})"),
        }
    }

    /// The first characters an address of this type starts with, for hints.
    pub fn prefix(self, network: Network) -> &'static str {
        match (self, network == Network::Bitcoin) {
            (Script::Legacy, true) => "1",
            (Script::Nested, true) => "3",
            (Script::Native, true) => "bc1q",
            (Script::Legacy, false) => "m or n",
            (Script::Nested, false) => "2",
            (Script::Native, false) if network == Network::Regtest => "bcrt1q",
            (Script::Native, false) => "tb1q",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_wrap_the_key() {
        assert_eq!(Script::Native.descriptor("K"), "wpkh(K)");
        assert_eq!(Script::Nested.descriptor("K"), "sh(wpkh(K))");
        assert_eq!(Script::Legacy.descriptor("K"), "pkh(K)");
    }
}
