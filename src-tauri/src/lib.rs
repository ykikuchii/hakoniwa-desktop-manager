mod commands;
mod core;
mod importer;
mod linking;
mod monitor;
mod process;
mod state;
mod types;

use state::AppState;
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

/// 起動直後に一括起動を行うコマンドライン引数。
///
/// デモや自動化で、画面上のボタンを座標クリックせずに一括起動するために使う。
/// ウィンドウ位置がずれるとクリックが別の場所に当たり、誤操作になるため。
const START_ALL_FLAG: &str = "--start-all";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .setup(|app| {
            if std::env::args().skip(1).any(|arg| arg == START_ALL_FLAG) {
                let handle = app.handle().clone();
                // 準備完了の待ち合わせでブロックするため、setup を止めないよう
                // 別スレッドで実行する。結果は画面側のポーリングで反映される。
                std::thread::spawn(move || {
                    if let Err(error) = commands::start_all(handle.state::<AppState>()) {
                        // release ビルドはコンソールを持たないので、失敗はダイアログで伝える。
                        eprintln!("{START_ALL_FLAG}: {error}");
                        handle
                            .dialog()
                            .message(error)
                            .kind(MessageDialogKind::Error)
                            .title("一括起動に失敗しました")
                            .show(|_| {});
                    }
                });
            }
            Ok(())
        })
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
