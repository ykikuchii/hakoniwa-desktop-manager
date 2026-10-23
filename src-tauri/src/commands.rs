use crate::{
    core::{install_core, load_catalog},
    importer::inspect_directory,
    process::{run_oneshot, ProcessError},
    state::AppState,
    types::{
        ActivationTiming, AssetDefinition, CommunicationEvent, CommunicationEventType,
        CoreCatalog, CoreInstallResult, EventDirection,
        ImportPreview, LifecycleCommandResult, ObservationSource, ProcessKind, ProcessStatus,
        ProgramSpec, ProjectSummary, ReadinessCheck, Workspace, WorkspaceSnapshot,
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
    state.release_if_idle();
    let mut store_warnings = Vec::new();
    // 開けないプロジェクト（参照先のアセットが隔離されたなど）でも画面は出す。
    // 空の構成を見せ、理由を警告に載せる。
    let mut workspace = state.run_workspace().unwrap_or_else(|error| {
        store_warnings.push(error);
        let (id, name) = state.lock_store().ok().and_then(|store| store.projects.get(store.active_project_id()).map(|project| (project.id.clone(), project.name.clone()))).unwrap_or_default();
        Workspace::empty(id, name)
    });
    // 保存済みの解決結果はキャッシュに過ぎない。アセットの追加・改名・削除に追従させるため、
    // 表示に渡すコピーの上で毎回引き直す（永続値はここでは書き換えない）。
    let _ = crate::linking::resolve_links(&workspace.assets, &mut workspace.imported_connections);
    let processes = state.processes.snapshots();
    harvest_monitor_logs(&state, &workspace, &processes);
    let (projects, active_project_id, members) = {
        let store = state.lock_store()?;
        store_warnings.splice(0..0, store.warnings.iter().cloned());
        let mut projects: Vec<ProjectSummary> = store.projects.values().map(|project| ProjectSummary { id: project.id.clone(), name: project.name.clone() }).collect();
        projects.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        let members = store.projects.get(store.active_project_id()).map(|project| project.members.iter().map(crate::manage::MemberSettings::of).collect()).unwrap_or_default();
        (projects, store.active_project_id().to_owned(), members)
    };
    Ok(WorkspaceSnapshot {
        workspace: workspace.clone(),
        platform: crate::types::HostPlatform::current(),
        architecture: crate::types::CpuArchitecture::current(),
        processes,
        connections: state.monitor.snapshots(&workspace.imported_connections),
        recent_events: state.monitor.recent_events(250),
        projects,
        active_project_id,
        running_project_id: state.running_project_id(),
        store_warnings,
        members,
    })
}

/// アクティブなプロジェクトを、編集した `Workspace` で置き換える。実行中なら拒否する。
/// `edit` は解決済みの構成を受け取って書き換える。
fn edit_active_workspace(state: &AppState, edit: impl FnOnce(&mut Workspace) -> Result<(), String>) -> Result<Workspace, String> {
    let project_id = state.lock_store()?.active_project_id().to_owned();
    let _gate = state.ensure_mutable(&project_id)?;
    let mut workspace = state.active_workspace()?;
    edit(&mut workspace)?;
    crate::edit::apply_workspace(&mut *state.lock_store()?, &project_id, &workspace)?;
    state.active_workspace()
}

#[tauri::command]
pub fn select_project(state: State<'_, AppState>, project_id: String) -> Result<(), String> {
    state.select_project(&project_id)
}

#[tauri::command]
pub fn create_project(state: State<'_, AppState>, name: String) -> Result<String, String> {
    if name.trim().is_empty() {
        return Err("プロジェクト名を指定してください。".to_owned());
    }
    let project = crate::model::Project::empty(Uuid::new_v4().to_string(), name.trim().to_owned());
    {
        let mut store = state.lock_store()?;
        store.save_project(&project)?;
        store.projects.insert(project.id.clone(), project.clone());
    }
    state.select_project(&project.id)?;
    Ok(project.id)
}

/// v1 の `workspace.json` を取り込む。`replace` なら同じ出所のプロジェクトを置き換える。
/// 取り込んだプロジェクトをアクティブにする。
#[tauri::command]
pub fn import_v1_workspace(state: State<'_, AppState>, path: String, replace: bool) -> Result<String, String> {
    import_v1(&state, Path::new(&path), replace)
}

pub fn import_v1(state: &AppState, path: &Path, replace: bool) -> Result<String, String> {
    // 置き換える相手が実行中なら拒否する。相手は取り込むまで分からないので、
    // 何かが実行中なら一律に拒否する。取り込んだ後でアクティブにも切り替える。
    state.release_if_idle();
    let gate = state.lock_gate();
    if state.start_all_running.load(Ordering::Acquire) || state.stop_all_running.load(Ordering::Acquire) || state.running_project_id().is_some() {
        return Err("実行中は取り込めません。すべて停止してから取り込んでください。".to_owned());
    }
    // 保存場所に置かれた v1 は、取り込んだら退避する。残すと毎回警告が出る。
    // 同じファイルかは、表記ではなくファイルの同一性で比べる。
    let requested = crate::store::file_identity(path);
    let data_dir_v1 = crate::store::file_identity(&state.data_directory.join(crate::store::V1_WORKSPACE_FILE));
    let consume = requested.is_some() && requested == data_dir_v1;
    let project_id = {
        let mut store = state.lock_store()?;
        // この起動の移行で、同じファイルを既に取り込んで退避している。取り込み直さず、
        // 移行で作ったプロジェクトを使う。
        let migrated = store.migrated_now.as_ref().filter(|(migrated, _)| requested.as_ref() == Some(migrated) && !path.exists()).map(|(_, id)| id.clone());
        match migrated {
            Some(id) => id,
            None => store.import_v1(path, replace, consume)?.project_id,
        }
    };
    drop(gate);
    state.select_project(&project_id)?;
    Ok(project_id)
}

#[tauri::command]
pub fn clear_store_warnings(state: State<'_, AppState>) -> Result<(), String> {
    state.lock_store()?.warnings.clear();
    Ok(())
}

#[tauri::command]
pub fn start_asset(state: State<'_, AppState>, asset_id: String) -> Result<crate::types::ProcessSnapshot, String> {
    // `enabled` は一括起動の対象かどうかだけを表す。外したアセットも単独では起動できる。
    // 一括起動と切り離して、実行中に後から足したり差し替えたりするアセットのため。
    // ただし一括起動・一括停止の最中は断る。一括停止が通り過ぎた後に起動して止めた
    // はずのものが残ったり、一括起動と二重に起動したりするため。
    // 確認からプロセスの登録まで `lifecycle_gate` を持ち、一括操作の開始と重ならないようにする。
    let gate = state.lock_gate();
    if state.start_all_running.load(Ordering::Acquire) || state.stop_all_running.load(Ordering::Acquire) {
        return Err("一括起動または一括停止の実行中は、アセットを単独で起動できません。".to_owned());
    }
    // 起動するのは実行中の構成（なければアクティブなプロジェクトを凍結したもの）から。
    let workspace = state.begin_run_locked()?;
    let started = workspace
        .assets
        .iter()
        .find(|candidate| candidate.id == asset_id)
        .cloned()
        .ok_or_else(|| "起動対象のアセットが見つかりません。".to_owned())
        .and_then(|asset| state.processes.start(asset.id, asset.name, ProcessKind::Asset, asset.command).map_err(|error| error.to_string()));
    drop(gate);
    // 起動に失敗して何も動いていなければ、用意した実行コンテキストを手放す。
    state.release_if_idle();
    started
}

// 停止後に Core の応答を確かめるため数秒ブロックしうる。UI を止めないよう async にする。
#[tauri::command(async)]
pub fn stop_asset(state: State<'_, AppState>, asset_id: String) -> Result<Vec<crate::types::ProcessSnapshot>, String> {
    let result = stop_asset_inner(state.clone(), asset_id);
    state.release_if_idle();
    result
}

fn stop_asset_inner(state: State<'_, AppState>, asset_id: String) -> Result<Vec<crate::types::ProcessSnapshot>, String> {
    let report = state.processes.stop_owner(&asset_id);
    if report.is_empty() { return Err("停止できる実行中プロセスがありません。".to_owned()); }
    if !report.failures.is_empty() {
        return Err(format!("停止できなかったプロセスがあります: {}", report.failures.join(" / ")));
    }
    // 単独停止は、ほかの参加者と master を動かしたまま 1 つだけを kill する。kill が
    // Core の master ロックを持っている最中に当たると、ロックのカウンタが戻らず残りの
    // 参加者が固まる（toppers/hakoniwa-core-cpp#62）。一括停止と違って master も
    // 消えないので、次の一括起動まで自然には直らない。ほかに動いているプロセスが
    // あるときだけ Core の応答を確かめ、固まっていそうなら利用者に知らせる。
    // ほかに何も動いていなければ master も無く、確かめても誤検知になるだけなので省く。
    let workspace = state.run_workspace()?;
    if !running_owner_names(&state, &workspace).is_empty() {
        let health = core_lock_health(&workspace);
        let name = report.stopped.first().map(|snapshot| snapshot.owner_name.clone()).unwrap_or_default();
        match health {
            // ABORT は Core のファイルに残り、すべて止めても `hako-cmd recover` を実行する
            // まで解けない。確認の間に残りが止まっていても知らせる。
            CoreLockHealth::Aborted { reason, owner_pid } => {
                return Err(format!(
                    "{name} を停止しました。Core は master ロックの持ち主（pid {owner_pid}）が終了したことを検出し、停止状態（ABORT）になっています（{reason}）。すべて停止し、hako-cmd recover を実行してから一括起動し直してください。"
                ));
            }
            // 疑いの段階の警告は、確認している間に一括停止などで残りが止まっていれば、
            // もう当てはまらない。
            CoreLockHealth::Stuck(detail) if !running_owner_names(&state, &workspace).is_empty() => {
                return Err(format!(
                    "{name} を停止しました。ただし Core が固まっている可能性があります（{detail}）。停止したプロセスが Core のロックを持ったまま終了した可能性があります。すべて停止してから一括起動し直してください。"
                ));
            }
            CoreLockHealth::Stuck(_) | CoreLockHealth::Free | CoreLockHealth::Unknown => {}
        }
    }
    Ok(report.stopped)
}

/// master ロックの空きを探す時間。正常な Core は conductor が周期ごとにロックを
/// 取って離すので、空きは最初の数十ミリ秒で見つかり、そこで打ち切る。
/// 長めにしているのは、生きている保持者が一時的に長くロックを持つ場合の誤警告を
/// 減らすため。Core 側に保持時間の上限は無いので、これは証明ではなく根拠にとどまる。
const LOCK_FREE_WINDOW: Duration = Duration::from_secs(6);
const LOCK_SAMPLE_INTERVAL: Duration = Duration::from_millis(10);

/// `hako-cmd status` で確かめるときの上限。正常なら 1 秒もかからない。
const CORE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// `hako-cmd check` の上限。読み取り専用で、既定ではロックを待たないので速い。
/// `check` を知らない古い hako-cmd は、Core が固まっていると返らないことがある。
const CORE_CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// Core の master ロックの状態。
#[derive(Debug, PartialEq, Eq)]
enum CoreLockHealth {
    /// 空きを観測した。または `hako-cmd check` が OK を返した。
    Free,
    /// `hako-cmd check` が、ロックの持ち主の終了を確認した（ABORT）。
    /// 新しい Core はこの状態で参加者を止め、`hako-cmd recover` を求める。
    Aborted { reason: String, owner_pid: i64 },
    /// 固まっている兆候があった。中身はその根拠。
    Stuck(String),
    /// 確かめられなかった。正常とも固まっているとも言えない。
    Unknown,
}

/// Core の master ロックの状態を確かめる。
///
/// 第一の手段は `flock.bin` の master カウンタ（先頭の int）を直接見ること。
/// ロックは `flock.bin` に置いたカウンタのセマフォで、1 以上が空きを表す。
/// `LOCK_FREE_WINDOW` の間に一度も空きが見えなければ固まっている兆候とする。
///
/// mmap の場所が分からない、または一度も読めなかった構成では `hako-cmd status` で
/// 代用する。ただし `status` は共有ログへの書き込みのときにしかロックを取らず、
/// ログが満杯だとそれも省かれるので、成功しても空きの証拠にはならない。代用で
/// 言えるのは「応答しなかった」場合だけで、成功時は `Unknown` とする。
fn core_lock_health(workspace: &Workspace) -> CoreLockHealth {
    let Some((selection, env)) = workspace.core_release.clone().map(|selection| (selection, workspace.core_env.clone())) else {
        return CoreLockHealth::Unknown;
    };
    // 新しい Core（toppers/hakoniwa-core-cpp#62 の修正以降）は `hako-cmd check` で、
    // ロックの持ち主が生きているかまで調べて答える。答えが出ればそれに従う。
    // UNKNOWN、`check` を知らない古い hako-cmd、時間切れのときは、下の直接観測に戻る。
    //
    // `check` を知らない古い hako-cmd に投げると、共有メモリに接続してログを書いて
    // から未知のコマンドと判定する。そのログ書き込みで master ロックを取るので、
    // 固まった Core では止まり、時間切れで kill するとロックを持ったまま終わりうる。
    // そこで先に、共有メモリに触れずに返る `--help` で `check` があるかを確かめる。
    let spec = |args: &[&str]| ProgramSpec { program: selection.hako_cmd_path.clone(), args: args.iter().map(|arg| (*arg).to_owned()).collect(), cwd: Some(selection.install_directory.clone()), env: env.clone(), target: crate::types::ExecutionTarget::Native };
    let supports_check = matches!(run_oneshot(&spec(&["--help"]), CORE_CHECK_TIMEOUT), Ok((_, stdout, _)) if help_lists_check(&stdout));
    if supports_check {
        if let Ok((_, stdout, _)) = run_oneshot(&spec(&["check"]), CORE_CHECK_TIMEOUT) {
            if let Some(health) = parse_core_check(&stdout) {
                return health;
            }
        }
    }
    let base = Path::new(&selection.install_directory);
    if let Some(flock) = core_flock_path(&env, base) {
        match master_lock_free_seen(&flock, LOCK_FREE_WINDOW) {
            Some(true) => return CoreLockHealth::Free,
            Some(false) => {
                return CoreLockHealth::Stuck(format!(
                    "{} 秒間、master ロックの空きが一度も観測されませんでした",
                    LOCK_FREE_WINDOW.as_secs()
                ))
            }
            None => {}
        }
    }
    let spec = ProgramSpec { program: selection.hako_cmd_path.clone(), args: vec!["status".to_owned()], cwd: Some(selection.install_directory.clone()), env, target: crate::types::ExecutionTarget::Native };
    match run_oneshot(&spec, CORE_PROBE_TIMEOUT) {
        Err(ProcessError::Timeout(_)) => CoreLockHealth::Stuck(format!("hako-cmd status が {} 秒以内に応答しませんでした", CORE_PROBE_TIMEOUT.as_secs())),
        Ok((_, stdout, stderr)) => stdout
            .lines()
            .chain(stderr.lines())
            .find(|line| line.contains("file-lock wait timed out"))
            .map_or(CoreLockHealth::Unknown, |line| CoreLockHealth::Stuck(line.trim().to_owned())),
        Err(_) => CoreLockHealth::Unknown,
    }
}

/// `hako-cmd check` の出力（1 行の JSON）を解釈する。
///
/// ABORT は `Aborted`。OK は、master ロックが UNLOCKED でカウンタが 1 以上のときだけ
/// `Free` とする。持ち主が生きていればロックを握ったまま止まっていても OK になるので、
/// LOCKED などの OK は判断せず、呼び出し側の直接観測に任せる。UNKNOWN、JSON でない
/// 出力、知らない `schema_version` も `None`。
fn parse_core_check(stdout: &str) -> Option<CoreLockHealth> {
    let value: serde_json::Value = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .and_then(|line| serde_json::from_str(line).ok())?;
    if value.get("schema_version")?.as_i64()? != 1 {
        return None;
    }
    let reason = value.get("reason").and_then(|reason| reason.as_str()).unwrap_or_default().to_owned();
    let master_lock = value
        .get("checks")
        .and_then(|checks| checks.as_array())
        .and_then(|checks| checks.iter().find(|check| check.get("name").and_then(|name| name.as_str()) == Some("master_lock")));
    match value.get("status")?.as_str()? {
        "OK" => {
            let lock = master_lock?;
            let unlocked = lock.get("state").and_then(|state| state.as_str()) == Some("UNLOCKED");
            let counter = lock.get("counter").and_then(|counter| counter.as_i64()).unwrap_or_default();
            (unlocked && counter >= 1).then_some(CoreLockHealth::Free)
        }
        "ABORT" => {
            let owner_pid = master_lock.and_then(|lock| lock.get("owner_pid")).and_then(|pid| pid.as_i64()).unwrap_or_default();
            Some(CoreLockHealth::Aborted { reason, owner_pid })
        }
        _ => None,
    }
}

/// `hako-cmd --help` の出力に `check` コマンドの行があるか。
fn help_lists_check(help: &str) -> bool {
    help.lines().any(|line| line.trim_start().split_whitespace().next() == Some("check") && line.starts_with(' '))
}

/// Core の設定（`HAKO_CONFIG_PATH`）から、mmap 構成の `flock.bin` の場所を求める。
///
/// 相対パスは `hako-cmd` を動かす作業ディレクトリ（`base`）を基準に解決する。
/// 設定ファイル側の `core_mmap_path` も同じ基準にそろえる。アセットごとに作業
/// ディレクトリが違う構成で相対パスを使うと、そもそも各プロセスが別の場所を
/// 見ることになるので、そこまでは補正しない。
fn core_flock_path(core_env: &BTreeMap<String, String>, base: &Path) -> Option<std::path::PathBuf> {
    let config_path = core_env.get("HAKO_CONFIG_PATH").cloned().or_else(|| std::env::var("HAKO_CONFIG_PATH").ok())?;
    let config_path = base.join(config_path);
    let config: serde_json::Value = serde_json::from_slice(&std::fs::read(config_path).ok()?).ok()?;
    if config.get("shm_type").and_then(|value| value.as_str()) != Some("mmap") {
        return None;
    }
    let directory = config.get("core_mmap_path")?.as_str()?;
    Some(base.join(directory).join("flock.bin"))
}

/// `window` の間に master カウンタが一度でも 1 以上（空き）だったか。
/// ファイルが開けない、または一度も読めなかったときは判断できないので `None`。
fn master_lock_free_seen(flock: &Path, window: Duration) -> Option<bool> {
    use std::io::Read;
    let deadline = Instant::now() + window;
    let mut read_any = false;
    while Instant::now() < deadline {
        // 読み取りは、カウンタを更新中のプロセスが掛ける排他ロックと衝突して失敗しうる。
        if let Ok(mut file) = std::fs::File::open(flock) {
            let mut bytes = [0u8; 4];
            if file.read_exact(&mut bytes).is_ok() {
                read_any = true;
                if i32::from_ne_bytes(bytes) >= 1 {
                    return Some(true);
                }
            }
        } else if !flock.exists() {
            return None;
        }
        thread::sleep(LOCK_SAMPLE_INTERVAL);
    }
    read_any.then_some(false)
}

#[tauri::command]
pub fn start_core_controller(state: State<'_, AppState>) -> Result<crate::types::ProcessSnapshot, String> {
    let gate = state.lock_gate();
    if state.start_all_running.load(Ordering::Acquire) || state.stop_all_running.load(Ordering::Acquire) {
        return Err("一括起動または一括停止の実行中は、Coreコントローラーを単独で起動できません。".to_owned());
    }
    let started = state.begin_run_locked().and_then(|workspace| {
        let controller = workspace.core_controller.ok_or_else(|| "Coreコントローラーを設定してください。".to_owned())?;
        spawn_controller(&state, controller)
    });
    drop(gate);
    state.release_if_idle();
    started
}

fn spawn_controller(state: &AppState, controller: crate::types::CoreController) -> Result<crate::types::ProcessSnapshot, String> {
    state.processes.start(controller.id, controller.name, ProcessKind::CoreController, controller.command).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn stop_core_controller(state: State<'_, AppState>) -> Result<Vec<crate::types::ProcessSnapshot>, String> {
    let controller = state.run_workspace()?.core_controller
        .ok_or_else(|| "Coreコントローラーを設定してください。".to_owned())?;
    let report = state.processes.stop_owner(&controller.id);
    state.release_if_idle();
    if report.is_empty() { return Err("停止できるCoreプロセスがありません。".to_owned()); }
    if !report.failures.is_empty() {
        return Err(format!("停止できなかったCoreプロセスがあります: {}", report.failures.join(" / ")));
    }
    Ok(report.stopped)
}

#[tauri::command]
pub fn run_lifecycle_command(state: State<'_, AppState>, command: String) -> Result<LifecycleCommandResult, String> {
    lifecycle(&state.run_workspace()?, command)
}

/// `workspace` の Core に `hako-cmd` を実行する。一括起動・一括停止は凍結した構成を渡す。
fn lifecycle(workspace: &Workspace, command: String) -> Result<LifecycleCommandResult, String> {
    if !matches!(command.as_str(), "start" | "stop" | "reset") {
        return Err("許可されていないライフサイクル操作です。".to_owned());
    }
    let selection = workspace.core_release.clone().ok_or_else(|| "承認済みCoreを導入して選択してください。".to_owned())?;
    let env = workspace.core_env.clone();
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
///
/// 一括停止から中断の指示が来ても、起動途中のアセットは準備完了まで
/// `CANCEL_SETTLE` を上限に待ってから中断する。登録や attach の最中に kill
/// すると、そのプロセスは Core の master ロックを持ったまま死に、カウンタが
/// 戻らず以後の参加者が固まる（toppers/hakoniwa-core-cpp#62）。準備完了は
/// その処理が終わった目印なので、そこまで待てば kill しても安全になる。
fn wait_for_readiness(
    state: &State<'_, AppState>,
    process_id: &str,
    owner_name: &str,
    check: &ReadinessCheck,
) -> Result<(), String> {
    if matches!(check, ReadinessCheck::Manual) {
        check_start_all_cancel(state)?;
        return Ok(());
    }
    let deadline = Instant::now() + READINESS_TIMEOUT;
    let mut cancel_deadline: Option<Instant> = None;
    let settle_error = || format!(
        "一括停止の指示により一括起動を中断しました（{owner_name} は {} 秒以内に準備完了せず、途中で停止します）。",
        CANCEL_SETTLE.as_secs()
    );
    loop {
        if state.start_all_cancel.load(Ordering::Acquire) {
            let settle_by = *cancel_deadline.get_or_insert_with(|| Instant::now() + CANCEL_SETTLE);
            if Instant::now() >= settle_by {
                return Err(settle_error());
            }
        }
        // 中断中は、TCP の確認も猶予の期限で打ち切る。名前解決だけは上限を掛けられない。
        let probe_deadline = cancel_deadline.map_or(deadline, |settle_by| settle_by.min(deadline));
        let ready = match check {
            ReadinessCheck::Manual => true,
            // ログ末尾は保持行数で押し出されるため、読み取り時点で記録した結果を見る。
            ReadinessCheck::LogContains { .. } => state.processes.marker_seen(process_id).map_err(|error| error.to_string())?,
            ReadinessCheck::TcpPort { host, port } => tcp_reachable(host, *port, probe_deadline),
        };
        // probe の後に状態を取り直す。probe の最中に終了したプロセスを見逃さない。
        let snapshot = state.processes.snapshot(process_id).map_err(|error| error.to_string())?;
        let terminated = matches!(snapshot.status, ProcessStatus::Exited | ProcessStatus::Failed);
        if ready && !terminated {
            // 準備完了まで待ったうえで、後続の起動はしない。
            check_start_all_cancel(state)?;
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
            // 中断中なら、通常のタイムアウトではなく中断として報告する。
            if cancel_deadline.is_some() {
                return Err(settle_error());
            }
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

/// アセット（一括起動の対象から外したものも含む）と Core コントローラーのうち、
/// プロセスが終了していないものの名前。
fn running_owner_names(state: &State<'_, AppState>, workspace: &Workspace) -> Vec<String> {
    let owners: BTreeSet<&str> = workspace
        .assets
        .iter()
        .map(|asset| asset.id.as_str())
        .chain(workspace.core_controller.iter().map(|controller| controller.id.as_str()))
        .collect();
    let names: BTreeSet<String> = state
        .processes
        .snapshots()
        .into_iter()
        .filter(|snapshot| owners.contains(snapshot.owner_id.as_str()))
        .filter(|snapshot| matches!(snapshot.status, ProcessStatus::Starting | ProcessStatus::Running | ProcessStatus::Stopping | ProcessStatus::Unknown))
        .map(|snapshot| snapshot.owner_name)
        .collect();
    names.into_iter().collect()
}

/// 一括停止から中断の指示が出ていればエラーにする。一括起動の区切りごとに呼ぶ。
fn check_start_all_cancel(state: &State<'_, AppState>) -> Result<(), String> {
    if state.start_all_cancel.load(Ordering::Acquire) {
        return Err("一括停止の指示により一括起動を中断しました。".to_owned());
    }
    Ok(())
}

/// 中断指示の後、起動途中のアセットの準備完了を待つ上限。
const CANCEL_SETTLE: Duration = Duration::from_secs(10);

/// 一括停止が起動の中断を待つ上限。起動側は起動途中のアセットの準備完了を
/// `CANCEL_SETTLE` まで待つので、それに確認間隔と TCP の 1 回の試行分を足す。
const START_ALL_CANCEL_WAIT: Duration = Duration::from_secs(15);

/// 一括停止の実行中フラグと中断指示を、関数を抜けるとき（失敗時も）に必ず下ろす。
struct StopAllGuard<'a>(&'a AppState);

impl Drop for StopAllGuard<'_> {
    fn drop(&mut self) {
        self.0.start_all_cancel.store(false, Ordering::Release);
        self.0.stop_all_running.store(false, Ordering::Release);
    }
}

/// `hako-cmd` の失敗のうち、Core が動いていないことを示すもの。停止の文脈では正常。
fn core_not_running(result: &LifecycleCommandResult) -> bool {
    result.stdout.contains("Not found hako-master") || result.stderr.contains("Not found hako-master")
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
    let result = start_all_inner(state.clone());
    // 起動に失敗して何も動いていなければ、実行コンテキストを手放す。部分的に起動した
    // ものが残っていれば保持し、次の一括停止が凍結した構成で止められるようにする。
    state.release_if_idle();
    result
}

fn start_all_inner(state: State<'_, AppState>) -> Result<Vec<crate::types::ProcessSnapshot>, String> {
    // フラグを立てるまで `lifecycle_gate` を持つ。単独起動の途中なら、その登録が
    // 終わってからここに来るので、下の稼働中の確認で見落とさない。
    let gate = state.lifecycle_gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.start_all_running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return Err("一括起動は既に実行中です。".to_owned());
    }
    let _guard = StartAllGuard(&state.start_all_running);
    // 実行コンテキストもゲートを持ったまま用意する。以後はこの凍結した構成だけを使う。
    let workspace = state.begin_run_locked()?;
    drop(gate);
    // フラグを立てた後に確認する。立てる前に確認すると、その隙に始まった停止を
    // 見落とす。停止側は起動のフラグが下りるまで待つので、どちらかが必ず譲る。
    if state.stop_all_running.load(Ordering::Acquire) {
        return Err("一括停止の実行中は一括起動できません。".to_owned());
    }
    // 接続チェックの Error と、承認した後に変わったメンバーがあれば起動しない。実行中の
    // プロジェクトは変更できないので、ここで見た内容のまま起動する。
    let reasons = {
        let (project, library) = check_snapshot(&state, state.running_project_id())?;
        crate::check::check_project(&project, &library)?.blocking_reasons()
    };
    if !reasons.is_empty() {
        return Err(format!("接続チェックで問題があるため一括起動しません。\n{}", reasons.join("\n")));
    }
    // 稼働中のアセットがあるまま一括起動すると、同じアセットが二重に起動する。
    // 登録アセットは Core に二重登録できず、外部アセットは同じポートを取り合う。
    // 一括起動の対象外でも、単独で起動したものが動いていれば同じ Core を使っているので数える。
    let alive = running_owner_names(&state, &workspace);
    if !alive.is_empty() {
        return Err(format!(
            "稼働中のプロセスがあるため一括起動できません（{}）。先にすべて停止してください。",
            alive.join("、")
        ));
    }
    let ordered = topological_order(&workspace.assets)?;
    let mut started = Vec::new();
    if let Some(controller) = workspace.core_controller.clone() {
        started.push(spawn_controller(&state, controller)?);
    }
    for timing in [ActivationTiming::BeforeStart, ActivationTiming::Manual, ActivationTiming::AfterStart] {
        for asset in ordered.iter().filter(|asset| asset.enabled && asset.activation_timing == timing) {
            check_start_all_cancel(&state)?;
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
            check_start_all_cancel(&state)?;
            // ここに到達した時点で、登録アセットはすべて準備完了を報告済み。
            // Core の起動で登録が締め切られても取りこぼしが出ない。
            // 失敗を握りつぶすと、PDU セグメントが無いまま after_start のアセットが
            // 起動され、原因と離れた場所（attach 時のクラッシュ）で症状が出る。
            let result = lifecycle(&workspace, "start".to_owned())?;
            if result.status == ProcessStatus::Failed {
                let detail = result.stderr.lines().chain(result.stdout.lines()).rev().find(|line| !line.trim().is_empty()).unwrap_or_default().to_owned();
                return Err(format!("hako-cmd start が失敗しました。{detail}"));
            }
        }
    }
    // 最後のアセットの起動直後に中断された場合も、成功として報告しない。
    check_start_all_cancel(&state)?;
    Ok(started)
}

// hako-cmd stop と各プロセスの停止待ちで数秒かかる。同期コマンドはメインスレッドで
// 走り、その間 UI が固まるため async 指定でワーカースレッドに逃がす。
#[tauri::command(async)]
pub fn stop_all(state: State<'_, AppState>) -> Result<Vec<crate::types::ProcessSnapshot>, String> {
    let result = stop_all_inner(state.clone());
    // 停止のフラグを下ろした後で確かめる。下ろす前は解放を見送るため。
    state.release_if_idle();
    result
}

fn stop_all_inner(state: State<'_, AppState>) -> Result<Vec<crate::types::ProcessSnapshot>, String> {
    // 単独起動の途中なら、その登録が終わってからフラグを立てる。下の停止で
    // そのプロセスも止める対象に入る。止める対象（実行中の構成）も、ゲートを持ったまま
    // 決めて固定する。重なった一括停止は拒否する。先に終わった方がフラグを下ろすと、
    // 実行中の構成が解放されて別のプロジェクトが起動でき、遅れた方がその同じ id の
    // プロセスを止めてしまうため。
    let workspace = {
        let _gate = state.lock_gate();
        if state.stop_all_running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return Err("一括停止は既に実行中です。".to_owned());
        }
        match state.begin_run_locked() {
            Ok(workspace) => workspace,
            Err(error) => {
                state.stop_all_running.store(false, Ordering::Release);
                return Err(error);
            }
        }
    };
    let _guard = StopAllGuard(&state);
    // 進行中の一括起動を止めてから停止する。止めないと、停止が通り過ぎた後で
    // 起動側が残りのアセットを起動し、停止したはずのものが動き続ける。
    state.start_all_cancel.store(true, Ordering::Release);
    let deadline = Instant::now() + START_ALL_CANCEL_WAIT;
    while state.start_all_running.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    let mut failures = Vec::new();
    if state.start_all_running.load(Ordering::Acquire) {
        failures.push(format!("一括起動が {} 秒以内に中断しませんでした。", START_ALL_CANCEL_WAIT.as_secs()));
    }
    if workspace.core_release.is_some() {
        // stop だけで登録アセットを kill すると、conductor を持つアセットが Core の
        // 状態遷移の途中で落ち、以後の hako-cmd が応答しなくなる（実測）。
        // stop → reset で登録アセットを hakopy.start() から戻し、自分で終了させてから
        // 残ったものだけを kill する。
        for command in ["stop", "reset"] {
            // Core が動いていなければ hako-cmd は失敗を返すが、停止としては正常。
            // それ以外の失敗と、応答しなかった場合は報告する。
            match lifecycle(&workspace, command.to_owned()) {
                Ok(result) if result.status == ProcessStatus::Failed && !core_not_running(&result) => {
                    let detail = result.stderr.lines().chain(result.stdout.lines()).rev().find(|line| !line.trim().is_empty()).unwrap_or_default().to_owned();
                    failures.push(format!("hako-cmd {command} が失敗しました。{detail}"));
                }
                Ok(_) => {}
                Err(error) => failures.push(error),
            }
        }
        let deadline = Instant::now() + STOP_GRACE;
        // 待つ対象は下の kill の対象と揃える。一括起動の対象外で単独起動したアセットも含む。
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
    let project_id = state.lock_store()?.active_project_id().to_owned();
    let _gate = state.ensure_mutable(&project_id)?;
    let mut workspace = state.active_workspace()?;
    workspace.source_directory = Some(preview.source_directory);
    workspace.assets = preview.assets;
    workspace.imported_connections = preview.connections;
    let mut store = state.lock_store()?;
    crate::edit::apply_workspace(&mut store, &project_id, &workspace)?;
    // 取り込みは利用者が構成を明示的に入れる操作なので、今の内容で承認する。
    store.accept(&project_id)?;
    drop(store);
    state.active_workspace()
}

/// 接続チェック用に、プロジェクトとライブラリを複製する。ファイルはロックの外で読むため。
/// `project_id` が `None` ならアクティブなプロジェクト。
fn check_snapshot(state: &AppState, project_id: Option<String>) -> Result<(crate::model::Project, crate::model::Library), String> {
    let store = state.lock_store()?;
    let id = project_id.unwrap_or_else(|| store.active_project_id().to_owned());
    let project = store.projects.get(&id).cloned().ok_or_else(|| "プロジェクトがありません。".to_owned())?;
    let library = project.members.iter().filter_map(|member| store.library.get(&member.asset_id).map(|asset| (asset.id.clone(), asset.clone()))).collect();
    Ok((project, library))
}

/// アクティブなプロジェクトの接続チェック。実行中なら、実行中のプロジェクトを見る。
// ファイルを読むので、UI を止めないよう async にする。
#[tauri::command(async)]
pub fn check_project(state: State<'_, AppState>) -> Result<crate::check::ProjectCheck, String> {
    let (project, library) = check_snapshot(&state, state.running_project_id())?;
    crate::check::check_project(&project, &library)
}

/// 画面が表示していた接続チェックのとおりに、メンバーを承認する。表示したプロジェクトが
/// アクティブでないか、表示した後に内容が変わっていれば拒否する。実行中も拒否する。
#[tauri::command(async)]
pub fn accept_member_changes(state: State<'_, AppState>, project_id: String, revisions: BTreeMap<String, String>) -> Result<crate::check::ProjectCheck, String> {
    const SWITCHED: &str = "表示中のプロジェクトが切り替わっています。接続チェックを見直してから承認してください。";
    if state.lock_store()?.active_project_id() != project_id {
        return Err(SWITCHED.to_owned());
    }
    // ファイルはロックの外で読む。読んでいる間にロックを持つと、画面の更新（ゲートを
    // 取る get_snapshot）まで待たされる。
    let (snapshot, library) = check_snapshot(&state, Some(project_id.clone()))?;
    let mut accepted = snapshot.clone();
    crate::check::accept_shown(&mut accepted, &library, &revisions)?;
    {
        let _gate = state.ensure_mutable(&project_id)?;
        let mut store = state.lock_store()?;
        if store.active_project_id() != project_id {
            return Err(SWITCHED.to_owned());
        }
        // 読んでいる間にプロジェクトかそのアセットが変わっていれば、承認しない。
        let unchanged = store.projects.get(&project_id).is_some_and(|current| serde_json::to_value(current).ok() == serde_json::to_value(&snapshot).ok())
            && library.iter().all(|(id, asset)| store.library.get(id).is_some_and(|current| serde_json::to_value(current).ok() == serde_json::to_value(asset).ok()));
        if !unchanged {
            return Err("承認している間にプロジェクトが変わりました。接続チェックを見直してから承認してください。".to_owned());
        }
        let draft = store.draft();
        store.commit(draft, accepted)?;
    }
    let (project, library) = check_snapshot(&state, Some(project_id))?;
    crate::check::check_project(&project, &library)
}

/// ライブラリのアセットをメンバーとして足したら、接続チェックがどうなるかを返す。保存しない。
// ファイルを読むので、UI を止めないよう async にする。
#[tauri::command(async)]
pub fn preview_add_member(state: State<'_, AppState>, project_id: String, member: crate::manage::MemberSettings) -> Result<crate::check::ProjectCheck, String> {
    let (project, library) = add_member_snapshot(&state, &project_id, &member.asset_id)?;
    let candidate = crate::manage::with_new_member(&project, member)?;
    crate::check::check_project(&candidate, &library)
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
    let selection = result.selection.clone();
    edit_active_workspace(&state, |workspace| {
        workspace.core_release = Some(selection);
        Ok(())
    })?;
    Ok(result)
}

#[tauri::command]
pub fn ingest_bridge_monitor_line(state: State<'_, AppState>, connection_id: String, line: String) -> Result<(), String> {
    let exists = state.run_workspace()?.imported_connections.iter().any(|connection| connection.id == connection_id);
    if !exists { return Err("接続定義が見つかりません。".to_owned()); }
    state.monitor.record_bridge_monitor_line(&connection_id, &line);
    Ok(())
}

#[tauri::command]
pub fn record_manual_communication_event(state: State<'_, AppState>, connection_id: String, message: String) -> Result<(), String> {
    let exists = state.run_workspace()?.imported_connections.iter().any(|connection| connection.id == connection_id);
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
        for connection in monitor_targets(asset, &workspace.assets, &workspace.imported_connections) {
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
/// `asset` のログを流し込む接続。解決済みの所有者が一致するもの。解決結果を持たない古い
/// 接続には Bridge 名の一致も使うが、同じ名前のアセットが複数あればどちらとも決めない。
fn monitor_targets<'a>(
    asset: &AssetDefinition,
    assets: &[AssetDefinition],
    connections: &'a [crate::types::ConnectionDefinition],
) -> Vec<&'a crate::types::ConnectionDefinition> {
    // linking と同じく、前後の空白を除いた名前で一意かを見る。
    let unique_name = assets.iter().filter(|candidate| candidate.name.trim() == asset.name.trim()).count() == 1;
    connections
        .iter()
        .filter(|connection| {
            connection.owner_asset_id.as_deref() == Some(asset.id.as_str())
                || (unique_name && connection.owner_asset_id.is_none() && connection.details.get("bridge").map(|bridge| bridge.trim() == asset.name.trim()).unwrap_or(false))
        })
        .collect()
}

fn topological_order(assets: &[AssetDefinition]) -> Result<Vec<AssetDefinition>, String> {
    let enabled: BTreeMap<String, AssetDefinition> = assets.iter().filter(|asset| asset.enabled).map(|asset| (asset.id.clone(), asset.clone())).collect();
    let names: BTreeMap<&str, &str> = assets.iter().map(|asset| (asset.id.as_str(), asset.name.as_str())).collect();
    // 必須の依存先を一括起動の対象から外していれば、黙って進めずに拒否する。外した
    // ものは一括起動では起動されないので待っても来ず、依存元は前提が無いまま動き出す。
    // 待たなくてよい依存は `optional_depends_on` に書く。
    for asset in enabled.values() {
        let excluded_required: Vec<&str> = asset.depends_on.iter()
            .filter(|dependency| dependency.as_str() != "core" && !enabled.contains_key(dependency.as_str()))
            .filter_map(|dependency| names.get(dependency.as_str()).copied())
            .collect();
        if !excluded_required.is_empty() {
            return Err(format!(
                "「{}」の必須の依存先（{}）が一括起動の対象から外れています。依存先を一括起動の対象に含めるか、任意の依存先に移してください。",
                asset.name,
                excluded_required.join("、")
            ));
        }
    }
    // 一括起動は起動タイミングの段階ごと（before_start → manual → after_start）に進み、
    // 段階の中だけを依存順に並べる。後の段階のアセットに依存すると、依存元が先に
    // 起動してしまい依存が満たせないので、何かを起動する前に拒否する。
    let phase = |timing: &ActivationTiming| match timing {
        ActivationTiming::BeforeStart => (0, "before_start"),
        ActivationTiming::Manual => (1, "manual"),
        ActivationTiming::AfterStart => (2, "after_start"),
    };
    for asset in enabled.values() {
        for dependency in asset.depends_on.iter().chain(asset.optional_depends_on.iter()) {
            // `core` は Core 本体を表す予約語。同じ ID のアセットがあっても段階の比較はしない。
            if dependency == "core" { continue; }
            let Some(target) = enabled.get(dependency.as_str()) else { continue };
            let (own_rank, own_label) = phase(&asset.activation_timing);
            let (target_rank, target_label) = phase(&target.activation_timing);
            if target_rank > own_rank {
                return Err(format!(
                    "「{}」（{own_label}）は、後の段階で起動する「{}」（{target_label}）に依存しています。一括起動は before_start → manual → after_start の順に進むため、この依存は満たせません。起動タイミングか依存先を見直してください。",
                    asset.name, target.name
                ));
            }
        }
    }
    // 任意の依存先は、対象外なら順序の制約にしない。ワークスペースに無い ID への
    // 依存は、必須・任意とも未解決参照として扱う。
    let excluded: BTreeSet<&str> = assets.iter().filter(|asset| !asset.enabled).map(|asset| asset.id.as_str()).collect();
    let mut ordered = Vec::new();
    let mut completed = BTreeSet::new();
    while completed.len() < enabled.len() {
        let ready: Vec<AssetDefinition> = enabled.values().filter(|asset| {
            !completed.contains(&asset.id)
                && asset.depends_on.iter().all(|dependency| dependency == "core" || completed.contains(dependency))
                && asset.optional_depends_on.iter().all(|dependency| dependency == "core" || excluded.contains(dependency.as_str()) || completed.contains(dependency))
        }).cloned().collect();
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
        AssetDefinition { id: id.to_owned(), name: id.to_owned(), role: AssetRole::Other, command: ProgramSpec { program: "echo".to_owned(), args: vec![], cwd: None, env: BTreeMap::new(), target: ExecutionTarget::Native }, depends_on: depends_on.into_iter().map(str::to_owned).collect(), optional_depends_on: vec![], activation_timing: ActivationTiming::Manual, config_files: vec![], enabled: true, readiness: ReadinessCheck::default() }
    }

    #[test]
    fn starts_dependencies_first() {
        let order = topological_order(&[asset("b", vec!["a"]), asset("a", vec![])]).unwrap();
        assert_eq!(order[0].id, "a");
    }

    /// 一括起動の対象から外したアセットは一括起動の順序に入らない。単独起動の可否とは無関係。
    #[test]
    fn start_all_order_skips_excluded_assets() {
        let mut excluded = asset("viewer-b", vec!["owner"]);
        excluded.enabled = false;
        let order = topological_order(&[asset("owner", vec![]), asset("viewer-a", vec!["owner"]), excluded]).unwrap();
        let ids: Vec<&str> = order.iter().map(|asset| asset.id.as_str()).collect();
        assert_eq!(ids, vec!["owner", "viewer-a"]);
    }

    /// 任意の依存先は、対象なら待ち、対象外なら待たない。
    #[test]
    fn start_all_order_waits_for_included_optional_dependencies_only() {
        let mut excluded = asset("viewer-b", vec![]);
        excluded.enabled = false;
        let mut pilot = asset("pilot", vec!["sim"]);
        pilot.optional_depends_on = vec!["viewer-a".to_owned(), "viewer-b".to_owned()];
        let order = topological_order(&[pilot, asset("viewer-a", vec!["sim"]), asset("sim", vec![]), excluded]).unwrap();
        let ids: Vec<&str> = order.iter().map(|asset| asset.id.as_str()).collect();
        assert_eq!(ids, vec!["sim", "viewer-a", "pilot"]);
    }

    /// 必須の依存先が対象外なら、理由を示して拒否する。
    #[test]
    fn start_all_order_rejects_excluded_required_dependency() {
        let mut excluded = asset("viewer-b", vec![]);
        excluded.enabled = false;
        let error = topological_order(&[asset("pilot", vec!["viewer-b"]), excluded]).unwrap_err();
        assert!(error.contains("pilot") && error.contains("viewer-b"), "{error}");
    }

    /// 後の段階で起動するアセットへの依存は、必須・任意とも拒否する。同じ段階や前の段階なら通す。
    #[test]
    fn start_all_order_rejects_dependencies_on_later_phases() {
        let mut early = asset("early", vec!["late"]);
        early.activation_timing = ActivationTiming::BeforeStart;
        let mut late = asset("late", vec![]);
        late.activation_timing = ActivationTiming::AfterStart;
        let error = topological_order(&[early.clone(), late.clone()]).unwrap_err();
        assert!(error.contains("early") && error.contains("late"), "{error}");

        early.depends_on.clear();
        early.optional_depends_on = vec!["late".to_owned()];
        assert!(topological_order(&[early.clone(), late.clone()]).is_err());

        let mut after = asset("after", vec!["early"]);
        after.activation_timing = ActivationTiming::AfterStart;
        early.optional_depends_on.clear();
        let order = topological_order(&[after, early]).unwrap();
        let ids: Vec<&str> = order.iter().map(|asset| asset.id.as_str()).collect();
        assert_eq!(ids, vec!["early", "after"]);
    }

    /// 予約語の `core` への依存は、同じ ID のアセットが後の段階にあっても拒否しない。
    #[test]
    fn start_all_order_does_not_phase_check_the_core_sentinel() {
        let mut core_asset = asset("core", vec![]);
        core_asset.activation_timing = ActivationTiming::AfterStart;
        let mut manual = asset("manual", vec!["core"]);
        manual.activation_timing = ActivationTiming::Manual;
        assert!(topological_order(&[manual, core_asset]).is_ok());
    }

    /// ワークスペースに無い ID への依存は、必須・任意とも拒否する。
    #[test]
    fn start_all_order_rejects_unknown_dependencies() {
        assert!(topological_order(&[asset("pilot", vec!["missing"])]).is_err());
        let mut pilot = asset("pilot", vec![]);
        pilot.optional_depends_on = vec!["missing".to_owned()];
        assert!(topological_order(&[pilot]).is_err());
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
        let matched = monitor_targets(&owner, std::slice::from_ref(&owner), &connections);
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
            monitor_targets(&owner, std::slice::from_ref(&owner), &connections).is_empty(),
            "解決を経ずに設定ファイルの一致だけで帰属させています。"
        );
    }

    /// 解決結果を持たない古いworkspace.jsonでも、Bridge名の一致では拾えること。
    #[test]
    fn monitor_targets_keep_bridge_name_fallback() {
        let owner = asset("pdu-bridge", vec![]);
        let mut legacy = connection("legacy");
        legacy.details.insert("bridge".to_owned(), "pdu-bridge".to_owned());
        assert_eq!(monitor_targets(&owner, std::slice::from_ref(&owner), &[legacy.clone()]).len(), 1);
    }

    /// 同じ名前のアセットが 2 つあると、Bridge 名ではどちらにも割り当てない。
    #[test]
    fn monitor_targets_skip_the_bridge_name_when_ambiguous() {
        let first = asset("pdu-bridge", vec![]);
        let mut second = asset(" pdu-bridge ", vec![]);
        second.id = "second-bridge".to_owned();
        let mut legacy = connection("legacy");
        legacy.details.insert("bridge".to_owned(), "pdu-bridge".to_owned());
        let assets = [first.clone(), second.clone()];
        assert!(monitor_targets(&first, &assets, std::slice::from_ref(&legacy)).is_empty());
        assert!(monitor_targets(&second, &assets, std::slice::from_ref(&legacy)).is_empty());
    }

    fn flock_with(name: &str, counter: i32) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("hdm-test-{name}-{}.bin", std::process::id()));
        let mut bytes = counter.to_ne_bytes().to_vec();
        bytes.extend_from_slice(&[0u8; 12]);
        std::fs::write(&path, bytes).expect("write flock");
        path
    }

    /// 空き（1）が見えれば固まっていない。
    #[test]
    fn lock_probe_sees_free_counter() {
        let path = flock_with("free", 1);
        assert_eq!(master_lock_free_seen(&path, Duration::from_millis(200)), Some(true));
        let _ = std::fs::remove_file(path);
    }

    /// 0 のまま空きが見えなければ固まっていると判断する。
    #[test]
    fn lock_probe_reports_stuck_counter() {
        let path = flock_with("stuck", 0);
        assert_eq!(master_lock_free_seen(&path, Duration::from_millis(200)), Some(false));
        let _ = std::fs::remove_file(path);
    }

    /// 相対の HAKO_CONFIG_PATH と core_mmap_path は hako-cmd の作業ディレクトリ基準で解決する。
    #[test]
    fn flock_path_resolves_relative_paths_against_base() {
        let base = std::env::temp_dir().join(format!("hdm-test-base-{}", std::process::id()));
        std::fs::create_dir_all(base.join("etc")).expect("create base");
        std::fs::write(base.join("etc").join("core.json"), r#"{"shm_type":"mmap","core_mmap_path":"mmap"}"#).expect("write config");
        let env = BTreeMap::from([("HAKO_CONFIG_PATH".to_owned(), "etc/core.json".to_owned())]);
        assert_eq!(core_flock_path(&env, &base), Some(base.join("mmap").join("flock.bin")));
        let _ = std::fs::remove_dir_all(base);
    }

    /// 絶対パスは基準ディレクトリに左右されない。shm 構成では flock.bin を探さない。
    #[test]
    fn flock_path_keeps_absolute_paths_and_skips_shm() {
        let dir = std::env::temp_dir().join(format!("hdm-test-abs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create dir");
        let mmap = dir.join("mmap");
        let config = dir.join("core.json");
        std::fs::write(&config, serde_json::json!({"shm_type": "mmap", "core_mmap_path": mmap.to_string_lossy()}).to_string()).expect("write config");
        let env = BTreeMap::from([("HAKO_CONFIG_PATH".to_owned(), config.to_string_lossy().into_owned())]);
        assert_eq!(core_flock_path(&env, Path::new("unrelated-base")), Some(mmap.join("flock.bin")));
        std::fs::write(&config, r#"{"shm_type":"shm"}"#).expect("rewrite config");
        assert_eq!(core_flock_path(&env, Path::new("unrelated-base")), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    // 以下の JSON は、修正後の Core（core-pro 56019b3 / core-cpp 3def26c）の
    // `hako-cmd check` が Windows で実際に出力したもの（pid などはその時の値）。
    const CHECK_OK: &str = r#"{"schema_version":1,"status":"OK","reason":"master lock is unlocked","checks":[{"name":"master_lock","status":"OK","reason":"master lock is unlocked","state":"UNLOCKED","counter":1,"owner_pid":0,"owner_start_token":0,"generation":23}]}"#;
    const CHECK_ABORT: &str = r#"{"schema_version":1,"status":"ABORT","reason":"master lock owner process is no longer alive","checks":[{"name":"master_lock","status":"ABORT","reason":"master lock owner process is no longer alive","state":"LOCKED","counter":0,"owner_pid":54416,"owner_start_token":134368578255130917,"generation":23}]}"#;

    /// OK は空き、ABORT は持ち主の pid と理由を持つ停止状態として読む。
    #[test]
    fn core_check_reads_ok_and_abort() {
        assert_eq!(parse_core_check(CHECK_OK), Some(CoreLockHealth::Free));
        assert_eq!(
            parse_core_check(&format!("INFO: something before
{CHECK_ABORT}
")),
            Some(CoreLockHealth::Aborted { reason: "master lock owner process is no longer alive".to_owned(), owner_pid: 54416 })
        );
    }

    /// 持ち主が生きたままロックを握っている OK（LOCKED、カウンタ 0）は空きとみなさない。
    #[test]
    fn core_check_does_not_treat_a_held_lock_as_free() {
        let held = CHECK_OK
            .replace(r#""state":"UNLOCKED","counter":1,"owner_pid":0"#, r#""state":"LOCKED","counter":0,"owner_pid":4242"#)
            .replace("master lock is unlocked", "master lock owner is alive");
        assert!(held.contains("LOCKED") && held.contains("4242"));
        assert_eq!(parse_core_check(&held), None);
    }

    /// `--help` に `check` の行があるかで、対応している hako-cmd かを見分ける。
    #[test]
    fn help_detects_the_check_command() {
        let fixed = "Usage: hako-cmd [OPTION...]\n      --force-wait           Allow check to wait for the OS file lock up to\n  start          Start simulation\n  check          Inspect runtime integrity as JSON\n  recover        Explicitly remove stopped runtime files\n";
        let legacy = "Usage: hako-cmd [OPTION...]\n  start          Start simulation\n  stop           Stop simulation\n";
        assert!(help_lists_check(fixed));
        assert!(!help_lists_check(legacy));
    }

    /// UNKNOWN、JSON でない出力、知らない版は判断せず、ほかの手段に任せる。
    #[test]
    fn core_check_leaves_unknown_and_foreign_output_undecided() {
        let unknown = CHECK_OK.replace(r#""status":"OK","reason":"master lock is unlocked","checks""#, r#""status":"UNKNOWN","reason":"master lock owner metadata is missing (legacy flock.bin)","checks""#);
        assert_eq!(parse_core_check(&unknown), None);
        assert_eq!(parse_core_check("Unknown command: check
Usage: hako-cmd ..."), None);
        assert_eq!(parse_core_check(""), None);
        assert_eq!(parse_core_check(&CHECK_OK.replace(r#""schema_version":1"#, r#""schema_version":2"#)), None);
    }

    /// ファイルが無ければ判断できない（警告しない）。
    #[test]
    fn lock_probe_without_file_is_inconclusive() {
        let path = std::env::temp_dir().join(format!("hdm-test-missing-{}.bin", std::process::id()));
        assert_eq!(master_lock_free_seen(&path, Duration::from_millis(200)), None);
    }
}

const SWITCHED: &str = "表示中のプロジェクトが切り替わっています。画面を読み直してから操作してください。";

/// アクティブで実行中でないプロジェクト `project_id` を、ゲートとストアのロックの下で編集する。
fn edit_project<T>(state: &AppState, project_id: &str, edit: impl FnOnce(&mut crate::store::Store) -> Result<T, String>) -> Result<T, String> {
    let _gate = state.ensure_mutable(project_id)?;
    let mut store = state.lock_store()?;
    if store.active_project_id() != project_id {
        return Err(SWITCHED.to_owned());
    }
    edit(&mut store)
}

/// 複製を取った後で、プロジェクトとそのアセットが変わっていないか。
fn snapshot_unchanged(store: &crate::store::Store, project: &crate::model::Project, library: &crate::model::Library) -> bool {
    store.projects.get(&project.id).is_some_and(|current| serde_json::to_value(current).ok() == serde_json::to_value(project).ok())
        && library.iter().all(|(id, asset)| store.library.get(id).is_some_and(|current| serde_json::to_value(current).ok() == serde_json::to_value(asset).ok()))
}

#[tauri::command]
pub fn get_library(state: State<'_, AppState>) -> Result<Vec<crate::manage::LibraryEntry>, String> {
    Ok(crate::manage::library_view(&*state.lock_store()?))
}

/// ライブラリのアセットを保存する。`apply_member_ids` が空でなければ、プロジェクト `project_id` の
/// それらのメンバー（`base_id` を使っているもの）を、保存した版に付け替える。
#[tauri::command]
pub fn save_library_asset(state: State<'_, AppState>, asset: crate::model::LibraryAsset, base_id: Option<String>, project_id: Option<String>, apply_member_ids: Vec<String>) -> Result<String, String> {
    match project_id.filter(|_| !apply_member_ids.is_empty()) {
        Some(project_id) => edit_project(&state, &project_id, |store| crate::manage::save_library_asset(store, asset, base_id.as_deref(), Some(crate::manage::Apply { project_id: &project_id, member_ids: &apply_member_ids }))),
        // プロジェクトを書き換えないので、実行中でも保存できる（実行中の構成は凍結済み）。
        None => crate::manage::save_library_asset(&mut *state.lock_store()?, asset, base_id.as_deref(), None),
    }
}

#[tauri::command]
pub fn delete_library_asset(state: State<'_, AppState>, asset_id: String) -> Result<(), String> {
    state.lock_store()?.remove_asset(&asset_id)
}

/// バリエーションの PDU 設定から読んだポート。保存しない（編集中の内容でも見られる）。
#[derive(serde::Serialize)]
pub struct VariantPorts {
    entries: Vec<crate::model::PduEntrySpec>,
    inferred: bool,
    channels: Vec<crate::model::ChannelUse>,
    problems: Vec<String>,
    coverage: crate::ports::Coverage,
}

// ファイルを読むので、UI を止めないよう async にする。
#[tauri::command(async)]
pub fn inspect_variant(variant: crate::model::Variant) -> Result<VariantPorts, String> {
    let (entries, inferred) = crate::check::effective_entries(&variant, &variant.activation_timing);
    let ports = crate::check::member_ports(&variant, &entries);
    Ok(VariantPorts { entries, inferred, channels: crate::check::channel_uses(&ports), problems: ports.problems, coverage: ports.coverage })
}

#[tauri::command]
pub fn update_member(state: State<'_, AppState>, project_id: String, expected: crate::manage::MemberSettings, member: crate::manage::MemberSettings) -> Result<(), String> {
    edit_project(&state, &project_id, |store| crate::manage::update_member(store, &project_id, &expected, member))
}

#[tauri::command]
pub fn set_member_asset(state: State<'_, AppState>, project_id: String, member_id: String, expected_asset_id: String, expected_variant_id: String, asset_id: String, variant_id: String) -> Result<(), String> {
    edit_project(&state, &project_id, |store| crate::manage::set_member_asset(store, &project_id, &member_id, &expected_asset_id, &expected_variant_id, &asset_id, &variant_id))
}

#[tauri::command]
pub fn remove_member(state: State<'_, AppState>, project_id: String, expected: crate::manage::MemberSettings) -> Result<(), String> {
    edit_project(&state, &project_id, |store| crate::manage::remove_member(store, &project_id, &expected))
}

/// メンバーを足す。`shown_revision` が、プレビューで表示したそのメンバーの revision と一致すれば
/// 承認済みで足す（利用者は所見とポートを見てから足している）。一致しなければ拒否する。
// ファイルを読むので、UI を止めないよう async にする。
#[tauri::command(async)]
pub fn add_member(state: State<'_, AppState>, project_id: String, member: crate::manage::MemberSettings, shown_revision: Option<String>) -> Result<(), String> {
    let (snapshot, library) = add_member_snapshot(&state, &project_id, &member.asset_id)?;
    let member_id = member.id.clone();
    let mut candidate = crate::manage::with_new_member(&snapshot, member)?;
    if let Some(revision) = shown_revision {
        crate::check::accept_shown(&mut candidate, &library, &BTreeMap::from([(member_id, revision)]))?;
    }
    edit_project(&state, &project_id, |store| {
        if !snapshot_unchanged(store, &snapshot, &library) {
            return Err("追加している間にプロジェクトかアセットが変わりました。プレビューを見直してから追加してください。".to_owned());
        }
        store.commit(store.draft(), candidate)
    })
}

/// プロジェクトとライブラリの複製に、足すアセットを加えたもの。
fn add_member_snapshot(state: &AppState, project_id: &str, asset_id: &str) -> Result<(crate::model::Project, crate::model::Library), String> {
    if state.lock_store()?.active_project_id() != project_id {
        return Err(SWITCHED.to_owned());
    }
    let (project, mut library) = check_snapshot(state, Some(project_id.to_owned()))?;
    let asset = state.lock_store()?.library.get(asset_id).cloned().ok_or_else(|| "そのアセットはライブラリにありません。".to_owned())?;
    library.insert(asset.id.clone(), asset);
    Ok((project, library))
}

#[tauri::command]
pub fn rename_project(state: State<'_, AppState>, project_id: String, expected: String, name: String) -> Result<(), String> {
    edit_project(&state, &project_id, |store| crate::manage::rename_project(store, &project_id, &expected, &name))
}

#[tauri::command]
pub fn set_core_controller(state: State<'_, AppState>, project_id: String, expected: Option<crate::types::CoreController>, controller: Option<crate::types::CoreController>) -> Result<(), String> {
    edit_project(&state, &project_id, |store| crate::manage::set_core_controller(store, &project_id, expected.as_ref(), controller))
}
