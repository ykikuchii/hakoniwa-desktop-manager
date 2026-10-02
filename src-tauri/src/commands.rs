use crate::{
    core::{install_core, load_catalog},
    importer::inspect_directory,
    process::run_oneshot,
    state::AppState,
    types::{
        ActivationTiming, AssetDefinition, CommunicationEvent, CommunicationEventType,
        CoreCatalog, CoreInstallResult, EventDirection,
        ImportPreview, LifecycleCommandResult, ObservationSource, ProcessKind, ProcessStatus,
        ProgramSpec, ReadinessCheck, Workspace, WorkspaceSnapshot,
    },
};
use chrono::Utc;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::{TcpStream, ToSocketAddrs},
    sync::atomic::{AtomicBool, Ordering},
    path::Path,
    thread,
    time::{Duration, Instant},
};
use tauri::State;
use uuid::Uuid;

#[tauri::command]
pub fn get_snapshot(state: State<'_, AppState>) -> Result<WorkspaceSnapshot, String> {
    let mut workspace = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?.clone();
    // 保存済みの解決結果はキャッシュに過ぎない。アセットの追加・改名・削除に追従させるため、
    // 表示に渡すコピーの上で毎回引き直す（永続値はここでは書き換えない）。
    let _ = crate::linking::resolve_links(&workspace.assets, &mut workspace.imported_connections);
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
    let report = state.processes.stop_owner(&asset_id);
    if report.is_empty() { return Err("停止できる実行中プロセスがありません。".to_owned()); }
    if !report.failures.is_empty() {
        return Err(format!("停止できなかったプロセスがあります: {}", report.failures.join(" / ")));
    }
    Ok(report.stopped)
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
    let report = state.processes.stop_owner(&controller.id);
    if report.is_empty() { return Err("停止できるCoreプロセスがありません。".to_owned()); }
    if !report.failures.is_empty() {
        return Err(format!("停止できなかったCoreプロセスがあります: {}", report.failures.join(" / ")));
    }
    Ok(report.stopped)
}

#[tauri::command]
pub fn run_lifecycle_command(state: State<'_, AppState>, command: String) -> Result<LifecycleCommandResult, String> {
    if !matches!(command.as_str(), "start" | "stop" | "reset") {
        return Err("許可されていないライフサイクル操作です。".to_owned());
    }
    let (selection, env) = {
        let workspace = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?;
        (workspace.core_release.clone(), workspace.core_env.clone())
    };
    let selection = selection.ok_or_else(|| "承認済みCoreを導入して選択してください。".to_owned())?;
    let spec = ProgramSpec { program: selection.hako_cmd_path, args: vec![command.clone()], cwd: Some(selection.install_directory), env, target: crate::types::ExecutionTarget::Native };
    let (code, stdout, stderr) = run_oneshot(&spec, LIFECYCLE_TIMEOUT).map_err(|error| format!("hako-cmd {command}: {error}"))?;
    Ok(LifecycleCommandResult { command, status: if code == 0 { ProcessStatus::Exited } else { ProcessStatus::Failed }, stdout, stderr })
}

/// `hako-cmd` の 1 回の操作の上限。正常なら 1 秒もかからない。
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(15);

/// `hako-cmd reset` のあと、登録アセットが自分で終了するのを待つ上限。
const STOP_GRACE: Duration = Duration::from_secs(3);

/// 準備完了を待つ上限。これを超えたら、条件の指定か対象そのものが誤っていると
/// みなして中断する。
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const READINESS_POLL: Duration = Duration::from_millis(200);

/// アセットが `readiness` の条件を満たすまで待つ。
///
/// `Manual`（既定）は待たない。従来の挙動と同じ。
///
/// 待つ意味があるのは、次の段階がこのアセットの準備完了を前提にしている場合。
/// Hakoniwa Core は `hako-cmd start` 以降はアセットの登録を受け付けないため、
/// 登録アセットが登録を終える前に Core を起動すると、後続のアセットは
/// `Can not register asset` で落ちる。プロセスを起動しただけでは、その登録が
/// 済んだことにはならない。
///
/// 対象プロセスが終了していれば、条件を満たしていても準備完了とはみなさない。
/// 別のプロセスが同じポートを開いている場合など、条件だけが偶然満たされる
/// ことがあるため。また、終了したプロセスはタイムアウトまで待たずに即座に
/// 失敗させる。本当の失敗理由がタイムアウトという別の症状に置き換わるため。
fn wait_for_readiness(
    state: &State<'_, AppState>,
    process_id: &str,
    owner_name: &str,
    check: &ReadinessCheck,
) -> Result<(), String> {
    if matches!(check, ReadinessCheck::Manual) {
        return Ok(());
    }
    let deadline = Instant::now() + READINESS_TIMEOUT;
    loop {
        let ready = match check {
            ReadinessCheck::Manual => true,
            // ログ末尾は保持行数で押し出されるため、読み取り時点で記録した結果を見る。
            ReadinessCheck::LogContains { .. } => state.processes.marker_seen(process_id).map_err(|error| error.to_string())?,
            ReadinessCheck::TcpPort { host, port } => tcp_reachable(host, *port, deadline),
        };
        // probe の後に状態を取り直す。probe の最中に終了したプロセスを見逃さない。
        let snapshot = state.processes.snapshot(process_id).map_err(|error| error.to_string())?;
        let terminated = matches!(snapshot.status, ProcessStatus::Exited | ProcessStatus::Failed);
        if ready && !terminated {
            return Ok(());
        }

        if terminated {
            // 最後の行をそのまま出すと、無害な警告が出力の末尾に来ているだけで
            // 本当の失敗理由が隠れる。エラーらしい行を後ろから探し、無ければ
            // 末尾に落とす。
            let looks_like_error = |line: &&String| {
                let line = line.as_str();
                ["ERROR", "Error", "error:", "Traceback", "FAILED", "Failed", "Exception"]
                    .iter()
                    .any(|needle| line.contains(needle))
            };
            let detail = snapshot
                .stderr_tail
                .iter()
                .chain(snapshot.stdout_tail.iter())
                .rev()
                .find(looks_like_error)
                .or_else(|| snapshot.stderr_tail.last())
                .or_else(|| snapshot.stdout_tail.last())
                .cloned()
                .unwrap_or_default();
            let code = snapshot
                .exit_code
                .map(|value| format!("終了コード {value}。"))
                .unwrap_or_default();
            return Err(format!(
                "{owner_name} は準備完了を報告する前に終了しました。{code}{detail}"
            ));
        }

        if Instant::now() >= deadline {
            return Err(match check {
                ReadinessCheck::LogContains { text } => format!(
                    "{owner_name} が {} 秒以内に「{text}」を出力しませんでした。",
                    READINESS_TIMEOUT.as_secs()
                ),
                ReadinessCheck::TcpPort { host, port } => format!(
                    "{owner_name} が {} 秒以内に {host}:{port} を開きませんでした。",
                    READINESS_TIMEOUT.as_secs()
                ),
                ReadinessCheck::Manual => unreachable!("Manual は待たずに返している"),
            });
        }

        thread::sleep(READINESS_POLL);
    }
}

/// 1 回の接続試行の上限。応答しないアドレスで待ち続けず、プロセスの終了確認へ戻る。
const TCP_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);

/// `host:port` に接続できるか。各試行は `deadline` までの残り時間と
/// `TCP_ATTEMPT_TIMEOUT` の短いほうで打ち切る。
///
/// 名前解決自体は OS の呼び出しで、ここでは上限を掛けられない。
fn tcp_reachable(host: &str, port: u16, deadline: Instant) -> bool {
    let Ok(addresses) = (host, port).to_socket_addrs() else { return false };
    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        if TcpStream::connect_timeout(&address, remaining.min(TCP_ATTEMPT_TIMEOUT)).is_ok() {
            return true;
        }
    }
    false
}

/// 一括起動の対象（有効なアセットと Core コントローラー）のうち、プロセスが
/// 終了していないものの名前。
fn running_owner_names(state: &State<'_, AppState>, workspace: &Workspace) -> Vec<String> {
    let owners: BTreeSet<&str> = workspace
        .assets
        .iter()
        .filter(|asset| asset.enabled)
        .map(|asset| asset.id.as_str())
        .chain(workspace.core_controller.iter().map(|controller| controller.id.as_str()))
        .collect();
    let names: BTreeSet<String> = state
        .processes
        .snapshots()
        .into_iter()
        .filter(|snapshot| owners.contains(snapshot.owner_id.as_str()))
        .filter(|snapshot| matches!(snapshot.status, ProcessStatus::Starting | ProcessStatus::Running | ProcessStatus::Stopping))
        .map(|snapshot| snapshot.owner_name)
        .collect();
    names.into_iter().collect()
}

/// 一括起動の実行中フラグを、関数を抜けるとき（失敗時も）に必ず下ろす。
struct StartAllGuard<'a>(&'a AtomicBool);

impl Drop for StartAllGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

// 準備完了の待ち合わせで数十秒ブロックしうる。同期コマンドはメインスレッドで
// 走り UI を止めるため、async 指定でワーカースレッドに逃がす。
#[tauri::command(async)]
pub fn start_all(state: State<'_, AppState>) -> Result<Vec<crate::types::ProcessSnapshot>, String> {
    if state.start_all_running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return Err("一括起動は既に実行中です。".to_owned());
    }
    let _guard = StartAllGuard(&state.start_all_running);
    let workspace = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?.clone();
    // 稼働中のアセットがあるまま一括起動すると、同じアセットが二重に起動する。
    // 登録アセットは Core に二重登録できず、外部アセットは同じポートを取り合う。
    let alive = running_owner_names(&state, &workspace);
    if !alive.is_empty() {
        return Err(format!(
            "稼働中のプロセスがあるため一括起動できません（{}）。先にすべて停止してください。",
            alive.join("、")
        ));
    }
    let ordered = topological_order(&workspace.assets)?;
    let mut started = Vec::new();
    if workspace.core_controller.is_some() {
        started.push(start_core_controller(state.clone())?);
    }
    for timing in [ActivationTiming::BeforeStart, ActivationTiming::Manual, ActivationTiming::AfterStart] {
        for asset in ordered.iter().filter(|asset| asset.enabled && asset.activation_timing == timing) {
            let marker = match &asset.readiness {
                ReadinessCheck::LogContains { text } => Some(text.clone()),
                _ => None,
            };
            let snapshot = state.processes.start_watching(asset.id.clone(), asset.name.clone(), ProcessKind::Asset, asset.command.clone(), marker).map_err(|error| error.to_string())?;
            // 依存順に起動しているので、次へ進む前にこのアセットの準備完了を待つ。
            // これを怠ると、後続のアセットや Core が、まだ準備できていない相手を
            // 前提に動き出す。
            wait_for_readiness(&state, &snapshot.id, &asset.name, &asset.readiness)?;
            started.push(snapshot);
        }
        if timing == ActivationTiming::BeforeStart && workspace.core_release.is_some() {
            // ここに到達した時点で、登録アセットはすべて準備完了を報告済み。
            // Core の起動で登録が締め切られても取りこぼしが出ない。
            // 失敗を握りつぶすと、PDU セグメントが無いまま after_start のアセットが
            // 起動され、原因と離れた場所（attach 時のクラッシュ）で症状が出る。
            let result = run_lifecycle_command(state.clone(), "start".to_owned())?;
            if result.status == ProcessStatus::Failed {
                let detail = result.stderr.lines().chain(result.stdout.lines()).rev().find(|line| !line.trim().is_empty()).unwrap_or_default().to_owned();
                return Err(format!("hako-cmd start が失敗しました。{detail}"));
            }
        }
    }
    Ok(started)
}

// hako-cmd stop と各プロセスの停止待ちで数秒かかる。同期コマンドはメインスレッドで
// 走り、その間 UI が固まるため async 指定でワーカースレッドに逃がす。
#[tauri::command(async)]
pub fn stop_all(state: State<'_, AppState>) -> Result<Vec<crate::types::ProcessSnapshot>, String> {
    let workspace = state.workspace.lock().map_err(|_| "ワークスペースをロックできません。".to_owned())?.clone();
    let mut failures = Vec::new();
    if workspace.core_release.is_some() {
        // stop だけで登録アセットを kill すると、conductor を持つアセットが Core の
        // 状態遷移の途中で落ち、以後の hako-cmd が応答しなくなる（実測）。
        // stop → reset で登録アセットを hakopy.start() から戻し、自分で終了させてから
        // 残ったものだけを kill する。
        for command in ["stop", "reset"] {
            // Core が動いていなければ hako-cmd は失敗を返すが、停止としては正常。
            // 報告するのは応答しなかった場合だけにする。
            if let Err(error) = run_lifecycle_command(state.clone(), command.to_owned()) {
                failures.push(error);
            }
        }
        let deadline = Instant::now() + STOP_GRACE;
        while !running_owner_names(&state, &workspace).is_empty() && Instant::now() < deadline {
            thread::sleep(READINESS_POLL);
        }
    }
    let mut stopped = Vec::new();
    for asset in workspace.assets.iter().rev() {
        let report = state.processes.stop_owner(&asset.id);
        stopped.extend(report.stopped);
        failures.extend(report.failures);
    }
    if let Some(controller) = workspace.core_controller {
        let report = state.processes.stop_owner(&controller.id);
        stopped.extend(report.stopped);
        failures.extend(report.failures);
    }
    if !failures.is_empty() {
        return Err(format!("停止できなかったプロセスがあります: {}", failures.join(" / ")));
    }
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
        for connection in monitor_targets(asset, &workspace.imported_connections) {
            for (index, line) in process.stdout_tail.iter().enumerate() {
                state.monitor.record_bridge_process_line(&connection.id, &process.id, "stdout", index, line);
            }
            for (index, line) in process.stderr_tail.iter().enumerate() {
                state.monitor.record_bridge_process_line(&connection.id, &process.id, "stderr", index, line);
            }
        }
    }
}

/// このアセットのログを、どの接続の観測情報として扱うか。
///
/// 帰属先は`linking`が解決した`owner_asset_id`を正とする。旧実装が併用していた
/// 「`endpoint_config`が`config_files`に含まれるか」は、importerが`config_files`へ
/// Launcher JSONのパスしか入れず`endpoint_config`にはendpoint/bridge JSONのパスしか
/// 入れないため恒常的に成立しなかったので落とした。Bridge名の一致は、解決結果を
/// 持たない古いworkspace.jsonのための後方互換として残す。
fn monitor_targets<'a>(
    asset: &AssetDefinition,
    connections: &'a [crate::types::ConnectionDefinition],
) -> Vec<&'a crate::types::ConnectionDefinition> {
    connections
        .iter()
        .filter(|connection| {
            connection.owner_asset_id.as_deref() == Some(asset.id.as_str())
                || connection.details.get("bridge").map(|bridge| bridge == &asset.name).unwrap_or(false)
        })
        .collect()
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
        AssetDefinition { id: id.to_owned(), name: id.to_owned(), role: AssetRole::Other, command: ProgramSpec { program: "echo".to_owned(), args: vec![], cwd: None, env: BTreeMap::new(), target: ExecutionTarget::Native }, depends_on: depends_on.into_iter().map(str::to_owned).collect(), activation_timing: ActivationTiming::Manual, config_files: vec![], enabled: true, readiness: ReadinessCheck::default() }
    }

    #[test]
    fn starts_dependencies_first() {
        let order = topological_order(&[asset("b", vec!["a"]), asset("a", vec![])]).unwrap();
        assert_eq!(order[0].id, "a");
    }

    fn connection(id: &str) -> crate::types::ConnectionDefinition {
        crate::types::ConnectionDefinition {
            id: id.to_owned(),
            source: "endpoint".to_owned(),
            destination: "external endpoint".to_owned(),
            label: format!("Endpoint: {id}"),
            transport: crate::types::TransportKind::Unknown,
            pdu_names: vec![],
            endpoint_config: None,
            details: BTreeMap::new(),
            source_asset_id: None,
            destination_asset_id: None,
            owner_asset_id: None,
        }
    }

    /// ログの帰属は解決済みのowner_asset_idで決まること。
    #[test]
    fn monitor_targets_follow_resolved_owner() {
        let owner = asset("bridge-asset", vec![]);
        let mut mine = connection("mine");
        mine.owner_asset_id = Some(owner.id.clone());
        let connections = [mine, connection("others")];
        let matched = monitor_targets(&owner, &connections);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "mine");
    }

    /// 旧実装の第2条件（endpoint_configがconfig_filesに含まれるか）は復活させない。
    /// importerの実際の入れ方では成立せず、成立するように見せかけると誤結合を招く。
    #[test]
    fn monitor_targets_ignore_config_file_overlap() {
        let mut owner = asset("path-asset", vec![]);
        owner.config_files = vec!["/recipe/endpoint_a.json".to_owned()];
        let mut candidate = connection("by-path");
        candidate.endpoint_config = Some("/recipe/endpoint_a.json".to_owned());
        let connections = [candidate];
        assert!(
            monitor_targets(&owner, &connections).is_empty(),
            "解決を経ずに設定ファイルの一致だけで帰属させています。"
        );
    }

    /// 解決結果を持たない古いworkspace.jsonでも、Bridge名の一致では拾えること。
    #[test]
    fn monitor_targets_keep_bridge_name_fallback() {
        let owner = asset("pdu-bridge", vec![]);
        let mut legacy = connection("legacy");
        legacy.details.insert("bridge".to_owned(), "pdu-bridge".to_owned());
        assert_eq!(monitor_targets(&owner, &[legacy]).len(), 1);
    }
}
