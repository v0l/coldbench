#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("coldbench {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--devices") {
        for d in hww::enumerate() {
            println!("{}\t{}\t{}", d.kind.label(), d.id, d.name);
        }
        return Ok(());
    }
    // --dir keeps wallets somewhere else, for testing or a second set.
    let dir = args
        .iter()
        .position(|a| a == "--dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(data_dir))
        .unwrap_or_else(|| PathBuf::from(".coldbench"));
    let tab = args
        .iter()
        .position(|a| a == "--tab")
        .and_then(|i| args.get(i + 1))
        .and_then(|t| coldbench::app::Tab::named(t));
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([1240.0, 800.0])
        .with_min_inner_size([900.0, 560.0])
        .with_title("coldbench")
        .with_app_id("coldbench")
        .with_drag_and_drop(true);
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "coldbench",
        options,
        Box::new(move |cc| {
            let args = std::env::args().collect::<Vec<_>>();
            egui_bench::install(&cc.egui_ctx);
            let mut app = coldbench::app::App::new(&cc.egui_ctx, dir);
            if let Some(t) = tab {
                app.tab = t;
            }
            // A PSBT named on the command line opens for review, which is how the suite hands
            // one over.
            if let Some(f) = args.iter().skip(1).find(|a| a.to_lowercase().ends_with(".psbt")) {
                match std::fs::read(f) {
                    Ok(b) => app.load_psbt(&b),
                    Err(e) => app.say(false, format!("{f}: {e}")),
                }
            }
            // Fills the send page with a privacy payment and builds it, for looking at the review
            // page without fee estimates or a device.
            if args.iter().any(|a| a == "--stonewall-demo") {
                app.demo_stonewall();
            }
            // Opens one transaction's details, for screenshots.
            if let Some(t) = args.iter().position(|a| a == "--tx").and_then(|i| args.get(i + 1)).and_then(|t| t.parse().ok()) {
                app.tab = coldbench::app::Tab::Transactions;
                app.selected_tx = Some(t);
            }
            if args.iter().any(|a| a == "--prompt") {
                app.preview_prompt();
            }
            if args.iter().any(|a| a == "--add") {
                app.new_wallet = Some(Default::default());
            }
            Ok(Box::new(app))
        }),
    )
}

/// `~/.local/share/coldbench`, moving the folder over from when the app was called wallet.
fn data_dir(base: PathBuf) -> PathBuf {
    let new = base.join("coldbench");
    let old = base.join("wallet");
    if !new.exists() && old.join("wallets").is_dir() && std::fs::rename(&old, &new).is_err() {
        return old;
    }
    new
}
