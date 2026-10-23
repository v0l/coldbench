//! Public Esplora and Electrum servers, grouped by who runs them.
//!
//! The scan spreads a wallet's addresses across operators, not across servers: blockstream.info
//! and electrum.blockstream.info see the same logs, so they count as one.

use bdk_wallet::bitcoin::Network;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Protocol {
    Esplora,
    /// Electrum over TLS.
    Electrum,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Server {
    pub protocol: Protocol,
    /// `https://host/api` for Esplora, `host:port` for Electrum.
    pub url: String,
    /// Who runs it. Servers with the same operator share one slice of the addresses.
    pub operator: String,
}

impl Server {
    pub fn esplora(url: &str, operator: &str) -> Server {
        Server { protocol: Protocol::Esplora, url: url.into(), operator: operator.into() }
    }

    pub fn electrum(url: &str, operator: &str) -> Server {
        Server { protocol: Protocol::Electrum, url: url.into(), operator: operator.into() }
    }

    /// Read what a person typed: `ssl://host:port` or `host:port` is Electrum, anything with
    /// `http` is Esplora. Each one typed by hand is its own operator.
    pub fn parse(text: &str) -> Option<Server> {
        let t = text.trim();
        if t.is_empty() {
            return None;
        }
        if t.starts_with("http://") || t.starts_with("https://") {
            return Some(Server::esplora(t.trim_end_matches('/'), t));
        }
        let hp = t.trim_start_matches("ssl://").trim_start_matches("tls://");
        let (host, port) = hp.rsplit_once(':')?;
        port.parse::<u16>().ok()?;
        (!host.is_empty()).then(|| Server::electrum(hp, hp))
    }

    pub fn label(&self) -> String {
        match self.protocol {
            Protocol::Esplora => self.url.clone(),
            Protocol::Electrum => format!("ssl://{}", self.url),
        }
    }

    pub fn host(&self) -> &str {
        let u = self.url.trim_start_matches("https://").trim_start_matches("http://");
        u.split(['/', ':']).next().unwrap_or(u)
    }
}

/// Servers that answered when this list was made (October 2026), most of them run by people
/// who publish who they are. Electrum servers with self-signed certificates are pinned on first
/// use.
pub fn public(network: Network) -> Vec<Server> {
    use Server as S;
    match network {
        Network::Bitcoin => vec![
            S::esplora("https://mempool.space/api", "mempool.space"),
            // blockstream.info's Esplora caps unauthenticated use at 700 requests an hour, so
            // only their Electrum server is used.
            S::electrum("electrum.blockstream.info:50002", "blockstream"),
            S::esplora("https://mempool.emzy.de/api", "emzy"),
            S::electrum("electrum.emzy.de:50002", "emzy"),
            S::esplora("https://mempool.ninja/api", "mempool.ninja"),
            S::esplora("https://mempool.bitaroo.net/api", "bitaroo"),
            S::electrum("electrum.bitaroo.net:50002", "bitaroo"),
            S::electrum("bitcoin.lukechilds.co:50002", "lukechilds"),
            S::electrum("electrum.diynodes.com:50002", "diynodes"),
            S::electrum("electrum.acinq.co:50002", "acinq"),
            S::electrum("btc.electroncash.dk:60002", "electroncash.dk"),
        ],
        Network::Testnet => vec![
            S::esplora("https://mempool.space/testnet/api", "mempool.space"),
            S::esplora("https://blockstream.info/testnet/api", "blockstream"),
            S::electrum("electrum.blockstream.info:60002", "blockstream"),
        ],
        Network::Testnet4 => vec![S::esplora("https://mempool.space/testnet4/api", "mempool.space")],
        Network::Signet => vec![S::esplora("https://mempool.space/signet/api", "mempool.space")],
        _ => vec![S::esplora("http://127.0.0.1:3002", "local")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_servers_are_read() {
        assert_eq!(Server::parse("ssl://a.b:50002").unwrap().protocol, Protocol::Electrum);
        assert_eq!(Server::parse("a.b:50002").unwrap().url, "a.b:50002");
        assert_eq!(Server::parse("https://x/api/").unwrap().url, "https://x/api");
        assert!(Server::parse("a.b").is_none());
        assert!(Server::parse("a.b:port").is_none());
        assert_eq!(Server::parse("https://x/api").unwrap().host(), "x");
        assert_eq!(Server::parse("a.b:50002").unwrap().host(), "a.b");
    }

    #[test]
    fn every_network_has_a_server_and_mainnet_has_many_operators() {
        for n in [Network::Bitcoin, Network::Testnet, Network::Testnet4, Network::Signet, Network::Regtest] {
            assert!(!public(n).is_empty());
        }
        let mut ops: Vec<String> = public(Network::Bitcoin).into_iter().map(|s| s.operator).collect();
        ops.sort();
        ops.dedup();
        assert!(ops.len() >= 8);
    }
}
