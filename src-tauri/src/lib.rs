mod commands;
mod compat;
mod core;
mod edit;
mod importer;
mod linking;
mod model;
mod monitor;
mod ports;
mod process;
mod resolve;
mod state;
mod store;
mod types;

use state::AppState;
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

/// 起動直後に一括起動を行うコマンドライン引数。
///
/// デモや自動化で、画面上のボタンを座標クリックせずに一括起動するために使う。
/// ウィンドウ位置がずれるとクリックが別の場所に当たり、誤操作になるため。
const START_ALL_FLAG: &str = "--start-all";

/// 起動直後に v1 の workspace.json を取り込む。同じ出所のプロジェクトは置き換える。
/// デモの生成スクリプトが書いた構成を、起動のたびに取り込み直すために使う。
const IMPORT_FLAG: &str = "--import";

/// 起動直後にアクティブにするプロジェクト（id か名前）。
const PROJECT_FLAG: &str = "--project";

#[derive(Debug, Default, PartialEq, Eq)]
struct Cli {
    import: Option<String>,
    project: Option<String>,
    start_all: bool,
}

fn parse_cli(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            START_ALL_FLAG => cli.start_all = true,
            IMPORT_FLAG => cli.import = Some(args.next().ok_or_else(|| format!("{IMPORT_FLAG} の後に workspace.json のパスを指定してください。"))?),
            PROJECT_FLAG => cli.project = Some(args.next().ok_or_else(|| format!("{PROJECT_FLAG} の後にプロジェクトの ID か名前を指定してください。"))?),
            _ => {}
        }
    }
    Ok(cli)
}

/// `--project` の値をプロジェクトの id に決める。id が一致すればそれを、なければ名前が
/// ちょうど 1 つ一致するものを使う。決められなければエラーにし、別のプロジェクトで
/// 一括起動してしまわないようにする。
fn resolve_project_arg(store: &store::Store, value: &str) -> Result<String, String> {
    if store.projects.contains_key(value) {
        return Ok(value.to_owned());
    }
    let matches: Vec<&model::Project> = store.projects.values().filter(|project| project.name.trim() == value.trim()).collect();
    match matches.as_slice() {
        [project] => Ok(project.id.clone()),
        [] => Err(format!("プロジェクト「{value}」が見つかりません。")),
        _ => Err(format!("「{value}」という名前のプロジェクトが {} つあります。ID で指定してください。", matches.len())),
    }
}

/// 一括起動の前の準備（取り込みとプロジェクトの選択）。
fn prepare(state: &AppState, cli: &Cli) -> Result<(), String> {
    if let Some(path) = &cli.import {
        commands::import_v1(state, std::path::Path::new(path), true)?;
    }
    if let Some(value) = &cli.project {
        let id = resolve_project_arg(&*state.lock_store()?, value)?;
        state.select_project(&id)?;
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .setup(|app| {
            let prepared = parse_cli(std::env::args().skip(1)).and_then(|cli| prepare(&app.state::<AppState>(), &cli).map(|()| cli));
            let cli = match prepared {
                Ok(cli) => cli,
                Err(error) => {
                    // 準備に失敗したら一括起動もしない。違う構成で起動しないため。
                    eprintln!("{error}");
                    app.dialog().message(error).kind(MessageDialogKind::Error).title("起動の準備に失敗しました").show(|_| {});
                    Cli::default()
                }
            };
            if cli.start_all {
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
            commands::select_project,
            commands::create_project,
            commands::import_v1_workspace,
            commands::clear_store_warnings,
        ])
        .run(tauri::generate_context!())
        .expect("Hakoniwa Desktop Managerを起動できませんでした。");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parses_the_launch_flags() {
        let cli = parse_cli(args(&["--import", "C:/demo/workspace.v1.json", "--project", "Gimbal x Drone demo", "--start-all"])).unwrap();
        assert_eq!(cli, Cli { import: Some("C:/demo/workspace.v1.json".to_owned()), project: Some("Gimbal x Drone demo".to_owned()), start_all: true });
        assert!(parse_cli(args(&["--project"])).is_err());
        assert!(parse_cli(args(&["--import"])).is_err());
        assert_eq!(parse_cli(args(&[])).unwrap(), Cli::default());
    }

    #[test]
    fn project_argument_must_match_exactly_one_project() {
        let dir = std::env::temp_dir().join(format!("hdm-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = store::Store::open(&dir);
        for (id, name) in [("a", "Demo"), ("b", "Demo"), ("c", "Solo")] {
            store.projects.insert(id.to_owned(), model::Project::empty(id.to_owned(), name.to_owned()));
        }
        assert_eq!(resolve_project_arg(&store, "c").unwrap(), "c");
        assert_eq!(resolve_project_arg(&store, "Solo").unwrap(), "c");
        assert_eq!(resolve_project_arg(&store, "a").unwrap(), "a", "id が一致すれば名前の重複は関係ない");
        assert!(resolve_project_arg(&store, "Demo").unwrap_err().contains("2 つ"));
        assert!(resolve_project_arg(&store, "Missing").is_err());
    }
}
