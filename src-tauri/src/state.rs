use crate::{monitor::CommunicationMonitor, process::ProcessManager, types::{CoreCatalog, Workspace}};
use std::{fs, path::PathBuf, sync::Mutex};
use uuid::Uuid;

pub struct AppState {
    pub workspace: Mutex<Workspace>,
    pub processes: ProcessManager,
    pub monitor: CommunicationMonitor,
    pub data_directory: PathBuf,
    pub catalog_path: PathBuf,
}

impl AppState {
    pub fn new() -> Self {
        let data_directory = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("HakoniwaDesktopManager");
        let catalog_path = data_directory.join("approved-core-catalog.json");
        let _ = fs::create_dir_all(&data_directory);
        ensure_default_catalog(&catalog_path);
        let workspace = load_workspace(&data_directory).unwrap_or_else(|| Workspace::empty(Uuid::new_v4().to_string(), "新しいHakoniwaワークスペース".to_owned()));
        Self { workspace: Mutex::new(workspace), processes: ProcessManager::new(), monitor: CommunicationMonitor::default(), data_directory, catalog_path }
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
