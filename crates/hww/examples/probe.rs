//! `cargo run -p hww --example probe` lists devices. With an index, opens that device and prints
//! its identity and the first receive address, saying what to do on the device as it goes.
use hww::{Notice, Passphrase, Prompt, Script, account_path};
use std::time::{Duration, Instant};

struct Say {
    started: Instant,
    limit: Duration,
}

impl Prompt for Say {
    fn notify(&self, n: Notice) {
        eprintln!("[{:>5.1}s] {n:?}", self.started.elapsed().as_secs_f32());
    }
    fn pin_matrix(&self) -> Option<String> {
        None
    }
    fn passphrase(&self) -> Option<Passphrase> {
        Some(Passphrase::Host(String::new()))
    }
    fn cancelled(&self) -> bool {
        self.started.elapsed() > self.limit
    }
}

fn main() {
    let t = Instant::now();
    let found = hww::enumerate();
    println!("{} device(s) in {:.1}s", found.len(), t.elapsed().as_secs_f32());
    for (i, f) in found.iter().enumerate() {
        println!("  {i}: {:?} {} ({})", f.kind, f.name, f.id);
    }
    let Some(i) = std::env::args().nth(1).and_then(|a| a.parse::<usize>().ok()) else { return };
    let secs = std::env::args().nth(2).and_then(|a| a.parse().ok()).unwrap_or(60);
    let say = Say { started: Instant::now(), limit: Duration::from_secs(secs) };
    let net = hww::bitcoin::Network::Bitcoin;
    let mut dev = match hww::open(&found[i], net, &say) {
        Ok(d) => d,
        Err(e) => return eprintln!("open failed: {e}"),
    };
    println!("{:#?}", dev.info());
    let path = account_path(Script::Native, net, 0);
    match dev.xpub(&path, &say) {
        Ok(x) => println!("{path} {x}"),
        Err(e) => eprintln!("xpub failed: {e}"),
    }
}
