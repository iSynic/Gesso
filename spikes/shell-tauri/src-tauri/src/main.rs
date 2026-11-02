//! Shell spike (b): Tauri 2 host. The document and canvas live in WASM inside the webview; Rust
//! here only serves font bytes, a ping for IPC latency, and writes the results file.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tauri::command]
fn read_font(name: String) -> tauri::ipc::Response {
    // Only plain file names from the system font folder (spike convenience, not shipped).
    let safe = !name.contains(['/', '\\', ':']) && !name.contains("..");
    let bytes = if safe { std::fs::read(format!("C:/Windows/Fonts/{name}")).unwrap_or_default() } else { Vec::new() };
    tauri::ipc::Response::new(bytes)
}

#[tauri::command]
fn ping(x: f64) -> f64 {
    x
}

#[tauri::command]
fn save_results(json: String) -> Result<String, String> {
    let path = std::env::var("SHELL_TAURI_OUT").unwrap_or_else(|_| "shell_tauri_results.json".into());
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path)
}

#[tauri::command]
fn auto_mode() -> bool {
    std::env::args().any(|a| a == "--auto")
}

#[tauri::command]
fn quit(app: tauri::AppHandle) {
    app.exit(0);
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![read_font, ping, save_results, auto_mode, quit])
        .run(tauri::generate_context!())
        .expect("tauri app");
}
