#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Connector mode: no window, MCP over stdio.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--mcp") {
        match args.get(i + 1) {
            Some(key_file) => aipg_desktop_lib::run_connector(key_file.clone()),
            None => eprintln!("usage: aipg-desktop --mcp <connector.json>"),
        }
        return;
    }
    aipg_desktop_lib::run();
}
