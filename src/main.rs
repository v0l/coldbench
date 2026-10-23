#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("wallet {}", env!("CARGO_PKG_VERSION"));
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
        .or_else(|| dirs::data_dir().map(|d| d.join("wallet")))
        .unwrap_or_else(|| PathBuf::from(".wallet"));
    let tab = args
        .iter()
        .position(|a| a == "--tab")
        .and_then(|i| args.get(i + 1))
        .and_then(|t| wallet::app::Tab::named(t));
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([1240.0, 800.0])
        .with_min_inner_size([900.0, 560.0])
        .with_title("wallet")
        .with_app_id("wallet")
        .with_drag_and_drop(true);
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "wallet",
        options,
        Box::new(move |cc| {
            let args = std::env::args().collect::<Vec<_>>();
            egui_bench::install(&cc.egui_ctx);
            let mut app = wallet::app::App::new(&cc.egui_ctx, dir);
            if let Some(t) = tab {
                app.tab = t;
            }
            if args.iter().any(|a| a == "--add") {
                app.new_wallet = Some(Default::default());
            }
            Ok(Box::new(app))
        }),
    )
}
