mod commands;
mod core;
mod importer;
mod linking;
mod monitor;
mod process;
mod state;
mod types;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::save_workspace,
            commands::create_asset,
            commands::update_asset,
            commands::delete_asset,
            commands::start_asset,
            commands::stop_asset,
            commands::start_core_controller,
            commands::stop_core_controller,
            commands::run_lifecycle_command,
            commands::start_all,
            commands::stop_all,
            commands::inspect_business_pack_directory,
            commands::apply_import_preview,
            commands::get_core_catalog,
            commands::save_core_catalog,
            commands::install_approved_core,
            commands::ingest_bridge_monitor_line,
            commands::record_manual_communication_event,
        ])
        .run(tauri::generate_context!())
        .expect("Hakoniwa Desktop Managerを起動できませんでした。");
}
