//! Print the txids of a wallet file's payments out, for `--tx` screenshots.
fn main() {
    let f = std::env::args().nth(1).expect("a wallet file");
    let a = coldbench::engine::Account::load(std::path::Path::new(&f)).unwrap();
    for r in a.txs().into_iter().filter(|r| r.delta < 0).take(5) {
        println!("{} {} in {} out", r.txid, r.tx.input.len(), r.tx.output.len());
    }
}
