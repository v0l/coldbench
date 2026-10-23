//! Talking to an Esplora server: scan, fees, broadcast. All calls block; the app runs them on a
//! thread.

use bdk_esplora::{EsploraExt, esplora_client};
use bdk_wallet::KeychainKind;
use bdk_wallet::bitcoin::{Network, Transaction};
use bdk_wallet::chain::spk_client::{FullScanRequest, FullScanResponse};

/// Public servers that need no account. Sparrow defaults to a public Electrum server; Esplora is
/// the same data over HTTPS.
/// The lowest rate Bitcoin Core relays since 29.1.
pub const MIN_RATE: f64 = 0.1;

pub fn default_server(network: Network) -> &'static str {
    match network {
        Network::Bitcoin => "https://mempool.space/api",
        Network::Testnet => "https://mempool.space/testnet/api",
        Network::Testnet4 => "https://mempool.space/testnet4/api",
        Network::Signet => "https://mempool.space/signet/api",
        _ => "http://127.0.0.1:3002",
    }
}

fn client(url: &str) -> esplora_client::BlockingClient {
    esplora_client::Builder::new(url.trim_end_matches('/')).timeout(30).build_blocking()
}

pub fn scan(
    url: &str,
    request: FullScanRequest<KeychainKind>,
    stop_gap: usize,
) -> Result<FullScanResponse<KeychainKind>, String> {
    client(url).full_scan(request, stop_gap, 4).map_err(|e| format!("{url}: {e}"))
}

pub fn broadcast(url: &str, tx: &Transaction) -> Result<(), String> {
    client(url).broadcast(tx).map_err(|e| format!("the server refused it: {e}"))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fees {
    /// Sat/vB for the next block, within three, within six and within a day.
    pub fast: f64,
    pub medium: f64,
    pub slow: f64,
    pub minimum: f64,
}

/// Asked directly rather than through `esplora_client`, which refuses mempool.space's answer
/// because it comes back as 203 rather than 200.
pub fn fees(url: &str) -> Result<Fees, String> {
    let at = format!("{}/fee-estimates", url.trim_end_matches('/'));
    let r = minreq::get(&at).with_timeout(20).send().map_err(|e| format!("{url}: {e}"))?;
    if !(200..300).contains(&r.status_code) {
        return Err(format!("{url}: answered {}", r.status_code));
    }
    let raw: std::collections::HashMap<String, f64> =
        serde_json::from_slice(r.as_bytes()).map_err(|e| format!("{url}: {e}"))?;
    let map = raw.into_iter().filter_map(|(k, v)| Some((k.parse().ok()?, v))).collect();
    Ok(pick(&map))
}

fn pick(map: &std::collections::HashMap<u16, f64>) -> Fees {
    // Esplora answers for targets in blocks. Use the nearest target at or above the one asked for.
    let at = |target: u16| -> f64 {
        let mut keys: Vec<&u16> = map.keys().collect();
        keys.sort();
        keys.into_iter().find(|k| **k >= target).or_else(|| map.keys().max()).and_then(|k| map.get(k)).copied().unwrap_or(1.0)
    };
    // Blocks clear below 1 sat/vB now; nodes relay down to 0.1.
    let floor = |v: f64| (v.max(MIN_RATE) * 10.0).round() / 10.0;
    Fees { fast: floor(at(1)), medium: floor(at(3)), slow: floor(at(6)), minimum: floor(at(1008)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_targets_pick_the_nearest_estimate() {
        let map = [(1u16, 40.0), (2, 30.0), (4, 20.0), (6, 12.0), (1008, 1.0)].into_iter().collect();
        let f = pick(&map);
        assert_eq!((f.fast, f.medium, f.slow, f.minimum), (40.0, 20.0, 12.0, 1.0));
    }

    #[test]
    fn an_empty_answer_still_gives_a_usable_rate() {
        let f = pick(&Default::default());
        assert_eq!(f.fast, MIN_RATE);
    }
}

#[cfg(test)]
mod live {
    #[test]
    #[ignore = "needs the network"]
    fn mempool_space_answers() {
        let f = super::fees("https://mempool.space/api");
        println!("{f:?}");
        assert!(f.is_ok());
    }
}
