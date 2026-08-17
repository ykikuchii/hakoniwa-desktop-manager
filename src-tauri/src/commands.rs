use crate::{
    core::{install_core, load_catalog},
    importer::inspect_directory,
    process::run_oneshot,
    state::AppState,
    types::{
        ActivationTiming, AssetDefinition, CommunicationEvent, CommunicationEventType,
        CoreCatalog, CoreInstallResult, EventDirection,
        ImportPreview, LifecycleCommandResult, ObservationSource, ProcessKind, ProcessStatus,
        ProgramSpec, Workspace, WorkspaceSnapshot,
    },
};
use chrono::Utc;
use std::{collections::{BTreeMap, BTreeSet}, path::Path};
use tauri::State;
use uuid::Uuid;

#[tauri::command]
pub fn get_snapshot(state: State<'_, AppState>) -> Result<WorkspaceSnapshot, String> {
    let workspace = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?.clone();
    let processes = state.processes.snapshots();
    harvest_monitor_logs(&state, &workspace, &processes);
    Ok(WorkspaceSnapshot {
        workspace: workspace.clone(),
        platform: crate::types::HostPlatform::current(),
        architecture: crate::types::CpuArchitecture::current(),
        processes,
        connections: state.monitor.snapshots(&workspace.imported_connections),
        recent_events: state.monitor.recent_events(250),
    })
}

#[tauri::command]
pub fn save_workspace(state: State<'_, AppState>, workspace: Workspace) -> Result<Workspace, String> {
    workspace.validate()?;
    {
        let mut stored = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?;
        *stored = workspace.clone();
    }
    state.persist_workspace()?;
    Ok(workspace)
}

#[tauri::command]
pub fn create_asset(state: State<'_, AppState>, asset: AssetDefinition) -> Result<Workspace, String> {
    asset.command.validate()?;
    let mut workspace = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?;
    if workspace.assets.iter().any(|candidate| candidate.id == asset.id) {
        return Err("同じアセットIDが既に存在します。".to_owned());
    }
    workspace.assets.push(asset);
    workspace.validate()?;
    let response = workspace.clone();
    drop(workspace);
    state.persist_workspace()?;
    Ok(response)
}

#[tauri::command]
pub fn update_asset(state: State<'_, AppState>, asset: AssetDefinition) -> Result<Workspace, String> {
    asset.command.validate()?;
    let mut workspace = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?;
    let existing = workspace.assets.iter_mut().find(|candidate| candidate.id == asset.id).ok_or_else(|| "更新対象のアセットが見つかりません。".to_owned())?;
    *existing = asset;
    workspace.validate()?;
    let response = workspace.clone();
    drop(workspace);
    state.persist_workspace()?;
    Ok(response)
}

#[tauri::command]
pub fn delete_asset(state: State<'_, AppState>, asset_id: String) -> Result<Workspace, String> {
    let mut workspace = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?;
    let before = workspace.assets.len();
    workspace.assets.retain(|asset| asset.id != asset_id);
    if before == workspace.assets.len() { return Err("削除対象のアセットが見つかりません。".to_owned()); }
    for asset in &mut workspace.assets { asset.depends_on.retain(|dependency| dependency != &asset_id); }
    let response = workspace.clone();
    drop(workspace);
    state.persist_workspace()?;
    Ok(response)
}

#[tauri::command]
pub fn start_asset(state: State<'_, AppState>, asset_id: String) -> Result<crate::types::ProcessSnapshot, String> {
    let asset = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?
        .assets.iter().find(|candidate| candidate.id == asset_id).cloned().ok_or_else(|| "起動対象のアセットが見つかりません。".to_owned())?;
    if !asset.enabled { return Err("このアセットは無効化されています。".to_owned()); }
    state.processes.start(asset.id, asset.name, ProcessKind::Asset, asset.command).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn stop_asset(state: State<'_, AppState>, asset_id: String) -> Result<Vec<crate::types::ProcessSnapshot>, String> {
    let snapshots = state.processes.stop_owner(&asset_id);
    if snapshots.is_empty() { return Err("停止できる実行中プロセスがありません。".to_owned()); }
    Ok(snapshots)
}

#[tauri::command]
pub fn start_core_controller(state: State<'_, AppState>) -> Result<crate::types::ProcessSnapshot, String> {
    let controller = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?.core_controller.clone()
        .ok_or_else(|| "Coreコントローラーを設定してください。".to_owned())?;
    state.processes.start(controller.id, controller.name, ProcessKind::CoreController, controller.command).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn stop_core_controller(state: State<'_, AppState>) -> Result<Vec<crate::types::ProcessSnapshot>, String> {
    let controller = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?.core_controller.clone()
        .ok_or_else(|| "Coreコントローラーを設定してください。".to_owned())?;
    let snapshots = state.processes.stop_owner(&controller.id);
    if snapshots.is_empty() { return Err("停止できるCoreプロセスがありません。".to_owned()); }
    Ok(snapshots)
}

#[tauri::command]
pub fn run_lifecycle_command(state: State<'_, AppState>, command: String) -> Result<LifecycleCommandResult, String> {
    if !matches!(command.as_str(), "start" | "stop" | "reset") {
        return Err("許可されていないライフサイクル操作です。".to_owned());
    }
    let selection = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?.core_release.clone()
        .ok_or_else(|| "承認済みCoreを導入して選択してください。".to_owned())?;
    let spec = ProgramSpec { program: selection.hako_cmd_path, args: vec![command.clone()], cwd: Some(selection.install_directory), env: BTreeMap::new(), target: crate::types::ExecutionTarget::Native };
    let (code, stdout, stderr) = run_oneshot(&spec).map_err(|error| error.to_string())?;
    Ok(LifecycleCommandResult { command, status: if code == 0 { ProcessStatus::Exited } else { ProcessStatus::Failed }, stdout, stderr })
}

#[tauri::command]
pub fn start_all(state: State<'_, AppState>) -> Result<Vec<crate::types::ProcessSnapshot>, String> {
    let workspace = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?.clone();
    let ordered = topological_order(&workspace.assets)?;
    let mut started = Vec::new();
    if workspace.core_controller.is_some() {
        started.push(start_core_controller(state.clone())?);
    }
    for timing in [ActivationTiming::BeforeStart, ActivationTiming::Manual, ActivationTiming::AfterStart] {
        for asset in ordered.iter().filter(|asset| asset.enabled && asset.activation_timing == timing) {
            started.push(state.processes.start(asset.id.clone(), asset.name.clone(), ProcessKind::Asset, asset.command.clone()).map_err(|error| error.to_string())?);
        }
        if timing == ActivationTiming::BeforeStart && workspace.core_release.is_some() {
            let _ = run_lifecycle_command(state.clone(), "start".to_owned());
        }
    }
    Ok(started)
}

#[tauri::command]
pub fn stop_all(state: State<'_, AppState>) -> Result<Vec<crate::types::ProcessSnapshot>, String> {
    let workspace = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?.clone();
    let _ = if workspace.core_release.is_some() { run_lifecycle_command(state.clone(), "stop".to_owned()) } else { Ok(LifecycleCommandResult { command: "stop".to_owned(), status: ProcessStatus::Unknown, stdout: String::new(), stderr: String::new() }) };
    let mut stopped = Vec::new();
    for asset in workspace.assets.iter().rev() { stopped.extend(state.processes.stop_owner(&asset.id)); }
    if let Some(controller) = workspace.core_controller { stopped.extend(state.processes.stop_owner(&controller.id)); }
    Ok(stopped)
}

#[tauri::command]
pub fn inspect_business_pack_directory(path: String) -> Result<ImportPreview, String> {
    inspect_directory(Path::new(&path))
}

#[tauri::command]
pub fn apply_import_preview(state: State<'_, AppState>, preview: ImportPreview) -> Result<Workspace, String> {
    let mut workspace = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?;
    workspace.source_directory = Some(preview.source_directory);
    workspace.assets = preview.assets;
    workspace.imported_connections = preview.connections;
    workspace.validate()?;
    let response = workspace.clone();
    drop(workspace);
    state.persist_workspace()?;
    Ok(response)
}

#[tauri::command]
pub fn get_core_catalog(state: State<'_, AppState>) -> Result<CoreCatalog, String> {
    load_catalog(&state.catalog_path).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn save_core_catalog(state: State<'_, AppState>, catalog: CoreCatalog) -> Result<CoreCatalog, String> {
    if catalog.schema_version != crate::types::CATALOG_SCHEMA_VERSION || catalog.component != "hakoniwa-core-pro" {
        return Err("承認済みCoreカタログの形式が正しくありません。".to_owned());
    }
    let content = serde_json::to_vec_pretty(&catalog).map_err(|error| error.to_string())?;
    let temporary = state.catalog_path.with_extension("json.tmp");
    std::fs::write(&temporary, content).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, &state.catalog_path).map_err(|error| error.to_string())?;
    Ok(catalog)
}

#[tauri::command]
pub fn install_approved_core(state: State<'_, AppState>, version: String) -> Result<CoreInstallResult, String> {
    let catalog = load_catalog(&state.catalog_path).map_err(|error| error.to_string())?;
    let result = install_core(&catalog, &version, &state.data_directory).map_err(|error| error.to_string())?;
    {
        let mut workspace = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?;
        workspace.core_release = Some(result.selection.clone());
    }
    state.persist_workspace()?;
    Ok(result)
}

#[tauri::command]
pub fn ingest_bridge_monitor_line(state: State<'_, AppState>, connection_id: String, line: String) -> Result<(), String> {
    let exists = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?.imported_connections.iter().any(|connection| connection.id == connection_id);
    if !exists { return Err("接続定義が見つかりません。".to_owned()); }
    state.monitor.record_bridge_monitor_line(&connection_id, &line);
    Ok(())
}

#[tauri::command]
pub fn record_manual_communication_event(state: State<'_, AppState>, connection_id: String, message: String) -> Result<(), String> {
    let exists = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?.imported_connections.iter().any(|connection| connection.id == connection_id);
    if !exists { return Err("接続定義が見つかりません。".to_owned()); }
    state.monitor.record(CommunicationEvent { id: Uuid::new_v4().to_string(), connection_id, observed_at: Utc::now(), direction: EventDirection::Bidirectional, event_type: CommunicationEventType::Heartbeat, pdu_name: None, byte_count: None, message, source: ObservationSource::Manual });
    Ok(())
}

fn harvest_monitor_logs(state: &AppState, workspace: &Workspace, processes: &[crate::types::ProcessSnapshot]) {
    for process in processes {
        let Some(asset) = workspace.assets.iter().find(|asset| asset.id == process.owner_id) else { continue; };
        if !matches!(asset.role, crate::types::AssetRole::Bridge | crate::types::AssetRole::Monitor) {
            continue;
        }
        let matched_connections: Vec<&crate::types::ConnectionDefinition> = workspace.imported_connections.iter().filter(|connection| {
            connection.details.get("bridge").map(|bridge| bridge == &asset.name).unwrap_or(false)
                || connection.endpoint_config.as_ref().map(|config| asset.config_files.iter().any(|asset_config| asset_config == config)).unwrap_or(false)
        }).collect();
        for connection in matched_connections {
            for (index, line) in process.stdout_tail.iter().enumerate() {
                state.monitor.record_bridge_process_line(&connection.id, &process.id, "stdout", index, line);
            }
            for (index, line) in process.stderr_tail.iter().enumerate() {
                state.monitor.record_bridge_process_line(&connection.id, &process.id, "stderr", index, line);
            }
        }
    }
}

fn topological_order(assets: &[AssetDefinition]) -> Result<Vec<AssetDefinition>, String> {
    let enabled: BTreeMap<String, AssetDefinition> = assets.iter().filter(|asset| asset.enabled).map(|asset| (asset.id.clone(), asset.clone())).collect();
    let mut ordered = Vec::new();
    let mut completed = BTreeSet::new();
    while completed.len() < enabled.len() {
        let ready: Vec<AssetDefinition> = enabled.values().filter(|asset| !completed.contains(&asset.id) && asset.depends_on.iter().all(|dependency| dependency == "core" || completed.contains(dependency))).cloned().collect();
        if ready.is_empty() {
            return Err("アセットの依存関係に循環または未解決参照があります。".to_owned());
        }
        for asset in ready { completed.insert(asset.id.clone()); ordered.push(asset); }
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AssetRole, ExecutionTarget, ProgramSpec};

    fn asset(id: &str, depends_on: Vec<&str>) -> AssetDefinition {
        AssetDefinition { id: id.to_owned(), name: id.to_owned(), role: AssetRole::Other, command: ProgramSpec { program: "echo".to_owned(), args: vec![], cwd: None, env: BTreeMap::new(), target: ExecutionTarget::Native }, depends_on: depends_on.into_iter().map(str::to_owned).collect(), activation_timing: ActivationTiming::Manual, config_files: vec![], enabled: true }
    }

    #[test]
    fn starts_dependencies_first() {
        let order = topological_order(&[asset("b", vec!["a"]), asset("a", vec![])]).unwrap();
        assert_eq!(order[0].id, "a");
    }
}
