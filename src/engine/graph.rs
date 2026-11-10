use super::{Account, analysis};
use bdk_wallet::KeychainKind;
use bdk_wallet::bitcoin::Txid;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub txid: Txid,
    pub label: String,
    pub time: Option<u64>,
    pub depth: usize,
    pub merges: usize,
    pub risks: usize,
    pub kyc: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub value: u64,
    pub change: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl History {
    pub fn columns(&self) -> usize {
        self.nodes.iter().map(|n| n.depth + 1).max().unwrap_or(0)
    }
}

impl Account {
    pub fn history_graph(&self) -> History {
        let mut rows: Vec<_> = self.wallet.transactions().collect();
        rows.sort_by_key(|t| (t.chain_position.confirmation_height_upper_bound().unwrap_or(u32::MAX), t.tx_node.txid));
        let index: HashMap<Txid, usize> = rows.iter().enumerate().map(|(i, t)| (t.tx_node.txid, i)).collect();
        let mut nodes: Vec<Node> = Vec::new();
        let mut edges = Vec::new();
        let mut depth: HashMap<usize, usize> = HashMap::new();
        let mut order: Vec<usize> = (0..rows.len()).collect();
        let mut placed: HashSet<usize> = HashSet::new();
        while placed.len() < rows.len() {
            let before = placed.len();
            for &i in &order {
                if placed.contains(&i) {
                    continue;
                }
                let tx = &rows[i].tx_node.tx;
                let parents: Vec<usize> = tx
                    .input
                    .iter()
                    .filter_map(|inp| index.get(&inp.previous_output.txid).copied())
                    .collect();
                if parents.iter().all(|p| placed.contains(p)) {
                    depth.insert(i, parents.iter().map(|p| depth[p] + 1).max().unwrap_or(0));
                    placed.insert(i);
                }
            }
            if placed.len() == before {
                break;
            }
        }
        order.retain(|i| placed.contains(i));
        order.sort_by_key(|i| depth[i]);
        let mut slot: HashMap<usize, usize> = HashMap::new();
        for &i in &order {
            let t = &rows[i];
            let tx = &t.tx_node.tx;
            let txid = t.tx_node.txid;
            let mut parents: HashSet<usize> = HashSet::new();
            let mut labels: HashSet<String> = HashSet::new();
            for inp in &tx.input {
                let Some(&p) = index.get(&inp.previous_output.txid) else { continue };
                if !placed.contains(&p) {
                    continue;
                }
                let Some(out) = rows[p].tx_node.tx.output.get(inp.previous_output.vout as usize) else { continue };
                if !self.wallet.is_mine(out.script_pubkey.clone()) {
                    continue;
                }
                parents.insert(p);
                let l = self.input_label(inp.previous_output);
                if !l.trim().is_empty() {
                    labels.insert(l.trim().to_string());
                }
            }
            let mut prevouts = Vec::new();
            for inp in &tx.input {
                if let Some(o) = self.wallet.tx_graph().get_txout(inp.previous_output) {
                    prevouts.push(o.clone());
                }
            }
            let own: Vec<bool> = tx.output.iter().map(|o| self.wallet.is_mine(o.script_pubkey.clone())).collect();
            let risks = if prevouts.len() == tx.input.len() {
                analysis::review(&analysis::Subject {
                    tx,
                    prevouts: &prevouts,
                    own_outputs: &own,
                    input_labels: &tx.input.iter().map(|i| self.input_label(i.previous_output)).collect::<Vec<_>>(),
                    reused_outputs: &[],
                })
                .iter()
                .filter(|f| f.severity == analysis::Severity::Warning)
                .count()
            } else {
                0
            };
            let id = nodes.len();
            slot.insert(i, id);
            nodes.push(Node {
                txid,
                label: self.label(&txid.to_string()).to_string(),
                time: match &t.chain_position {
                    bdk_wallet::chain::ChainPosition::Confirmed { anchor, .. } => Some(anchor.confirmation_time),
                    bdk_wallet::chain::ChainPosition::Unconfirmed { first_seen, .. } => *first_seen,
                },
                depth: depth[&i],
                merges: parents.len(),
                risks,
                kyc: labels.iter().any(|l| analysis::is_kyc_label(l)),
            });
            for inp in &tx.input {
                let Some(&p) = index.get(&inp.previous_output.txid) else { continue };
                let Some(&from) = slot.get(&p) else { continue };
                let Some(out) = rows[p].tx_node.tx.output.get(inp.previous_output.vout as usize) else { continue };
                let Some((k, _)) = self.wallet.derivation_of_spk(out.script_pubkey.clone()) else { continue };
                edges.push(Edge { from, to: id, value: out.value.to_sat(), change: k == KeychainKind::Internal });
            }
        }
        History { nodes, edges }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::*;
    use super::super::*;

    #[test]
    fn merging_two_funded_coins_shows_as_a_merge_with_a_kyc_source() {
        let dir = tmp("graph");
        let mut a = account(Script::Native, Network::Bitcoin, &dir);
        let one = a.receive().address;
        let t1 = fund(&mut a, &one, 60_000, 100);
        let two = a.receive().address;
        let t2 = fund(&mut a, &two, 60_000, 101);
        a.set_label(&t1.to_string(), "exchange");
        a.set_label(&t2.to_string(), "salary");
        let to = a.parse_address("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq").unwrap();
        let built = a
            .build(&SendPlan { recipients: vec![(to, 90_123)], sat_per_vb: 3.0, rbf: true, ..Default::default() })
            .unwrap();
        let mut psbt = built.psbt.clone();
        psbt.sign(&master(Network::Bitcoin), &bdk_wallet::bitcoin::secp256k1::Secp256k1::new()).unwrap();
        let tx = a.finalize(&psbt).unwrap();
        a.add_sent(&tx, "rent").unwrap();

        let g = a.history_graph();
        assert_eq!(g.nodes.len(), 3);
        let spend = g.nodes.iter().find(|n| n.label == "rent").unwrap();
        assert_eq!(spend.merges, 2);
        assert!(spend.kyc);
        assert_eq!(spend.depth, 1);
        assert_eq!(g.edges.len(), 2);
        assert!(g.edges.iter().all(|e| g.nodes[e.to].label == "rent" && !e.change));
        assert_eq!(g.columns(), 2);
    }
}
