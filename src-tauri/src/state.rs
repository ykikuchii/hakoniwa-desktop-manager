use crate::{monitor::CommunicationMonitor, process::ProcessManager, types::{CoreCatalog, Workspace}};
use std::{fs, path::PathBuf, sync::{atomic::AtomicBool, Mutex}};
use uuid::Uuid;

pub struct AppState {
    pub workspace: Mutex<Workspace>,
    pub processes: ProcessManager,
    pub monitor: CommunicationMonitor,
    pub data_directory: PathBuf,
    pub catalog_path: PathBuf,
    /// 一括起動の実行中フラグ。起動時の`--start-all`と画面のボタンが重なると、
    /// 同じアセットが二重に起動され`hako-cmd start`も二度走るため、重複を拒否する。
    pub start_all_running: AtomicBool,
    /// 一括停止から一括起動への中断指示。起動は区切りごとにこれを見て止まる。
    pub start_all_cancel: AtomicBool,
    /// 一括停止の実行中フラグ。停止中に始まった一括起動を拒否する。
    pub stop_all_running: AtomicBool,
}

impl AppState {
    pub fn new() -> Self {
        let data_directory = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("HakoniwaDesktopManager");
        let catalog_path = data_directory.join("approved-core-catalog.json");
        let _ = fs::create_dir_all(&data_directory);
        ensure_default_catalog(&catalog_path);
        let workspace = load_workspace(&data_directory).unwrap_or_else(|| Workspace::empty(Uuid::new_v4().to_string(), "新しいHakoniwaワークスペース".to_owned()));
        Self { workspace: Mutex::new(workspace), processes: ProcessManager::new(), monitor: CommunicationMonitor::default(), data_directory, catalog_path, start_all_running: AtomicBool::new(false), start_all_cancel: AtomicBool::new(false), stop_all_running: AtomicBool::new(false) }
    }

    pub fn persist_workspace(&self) -> Result<(), String> {
        let workspace = self.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?;
        workspace.validate()?;
        let target = self.data_directory.join("workspace.json");
        let temporary = self.data_directory.join("workspace.json.tmp");
        let content = serde_json::to_vec_pretty(&*workspace).map_err(|error| error.to_string())?;
        fs::write(&temporary, content).map_err(|error| error.to_string())?;
        fs::rename(&temporary, &target).map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn load_workspace(data_directory: &PathBuf) -> Option<Workspace> {
    let path = data_directory.join("workspace.json");
    let workspace = serde_json::from_slice::<Workspace>(&fs::read(path).ok()?).ok()?;
    workspace.validate().ok()?;
    Some(workspace)
}

fn ensure_default_catalog(path: &PathBuf) {
    if path.is_file() { return; }
    let catalog = CoreCatalog {
        schema_version: crate::types::CATALOG_SCHEMA_VERSION,
        component: "hakoniwa-core-pro".to_owned(),
        publisher: "Hakoniwa Desktop Manager maintainers".to_owned(),
        releases: Vec::new(),
    };
    if let Ok(content) = serde_json::to_vec_pretty(&catalog) {
        let _ = fs::write(path, content);
    }
}
