use crate::{
    monitor::CommunicationMonitor,
    process::ProcessManager,
    resolve::resolve,
    store::Store,
    types::{CoreCatalog, ProcessStatus, Workspace},
};
use std::{
    fs,
    path::PathBuf,
    sync::{atomic::AtomicBool, atomic::Ordering, Mutex, MutexGuard},
};

pub struct AppState {
    /// ライブラリとプロジェクト（保存の正本）。
    pub store: Mutex<Store>,
    /// 実行中のプロジェクトの、凍結した解決結果。起動から、そのプロセスがすべて止まる
    /// まで持つ。起動と停止に関わる処理は、編集中の構成ではなくこれを使う。
    pub run: Mutex<Option<RunContext>>,
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
    /// 実行中のプロジェクトで `hako-cmd start` が成功し、まだ止めていないか。Core コントローラーの
    /// ないプロジェクトでは、Core の状態はこれでしか分からない。
    pub core_started: AtomicBool,
    /// 単独起動の受付と、一括起動・一括停止の開始を直列化する。単独起動は確認から
    /// プロセスの登録までこれを持つので、その間に一括停止が通り過ぎることも、
    /// 一括起動が稼働中の確認を終えてしまうこともない。実行コンテキストの生成・解放と、
    /// プロジェクトの変更・切り替えの可否の判定も、これを持って行う。
    pub lifecycle_gate: Mutex<()>,
}

#[derive(Debug, Clone)]
pub struct RunContext {
    pub project_id: String,
    pub workspace: Workspace,
}

impl AppState {
    pub fn new() -> Self {
        let data_directory = data_directory();
        let catalog_path = data_directory.join("approved-core-catalog.json");
        let _ = fs::create_dir_all(&data_directory);
        ensure_default_catalog(&catalog_path);
        let store = Store::open(&data_directory);
        Self::with_store(store, data_directory, catalog_path)
    }

    pub fn with_store(store: Store, data_directory: PathBuf, catalog_path: PathBuf) -> Self {
        Self {
            store: Mutex::new(store),
            run: Mutex::new(None),
            processes: ProcessManager::new(),
            monitor: CommunicationMonitor::default(),
            data_directory,
            catalog_path,
            start_all_running: AtomicBool::new(false),
            start_all_cancel: AtomicBool::new(false),
            stop_all_running: AtomicBool::new(false),
            core_started: AtomicBool::new(false),
            lifecycle_gate: Mutex::new(()),
        }
    }

    pub fn lock_store(&self) -> Result<MutexGuard<'_, Store>, String> {
        self.store.lock().map_err(|_| "保存データをロックできません。".to_owned())
    }

    fn lock_run(&self) -> MutexGuard<'_, Option<RunContext>> {
        self.run.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn lock_gate(&self) -> MutexGuard<'_, ()> {
        self.lifecycle_gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// アクティブなプロジェクトを解決する。表示と編集の前提に使う。
    pub fn active_workspace(&self) -> Result<Workspace, String> {
        let store = self.lock_store()?;
        let id = store.active_project_id();
        let project = store.projects.get(id).ok_or_else(|| "アクティブなプロジェクトがありません。".to_owned())?;
        resolve(project, &store.library).map_err(|error| format!("プロジェクト「{}」を開けません: {error}", project.name))
    }

    /// 実行中ならその凍結した構成を、そうでなければアクティブなプロジェクトを返す。
    /// 起動と停止に関わる処理は、すべてこちらを使う。
    pub fn run_workspace(&self) -> Result<Workspace, String> {
        if let Some(run) = self.lock_run().as_ref() {
            return Ok(run.workspace.clone());
        }
        self.active_workspace()
    }

    pub fn running_project_id(&self) -> Option<String> {
        self.lock_run().as_ref().map(|run| run.project_id.clone())
    }

    /// 起動の前に、実行コンテキストを用意する。呼び出し側が `lifecycle_gate` を持っていること。
    ///
    /// 既にあれば、それがアクティブなプロジェクトのものである限りそのまま使う（実行中は
    /// そのプロジェクトを変更できないので、凍結した構成は今の構成と同じ）。
    pub fn begin_run_locked(&self) -> Result<Workspace, String> {
        let active_id = self.lock_store()?.active_project_id().to_owned();
        let mut run = self.lock_run();
        if let Some(context) = run.as_ref() {
            if context.project_id != active_id {
                return Err("別のプロジェクトが実行中です。先にすべて停止してください。".to_owned());
            }
            return Ok(context.workspace.clone());
        }
        let workspace = self.active_workspace()?;
        *run = Some(RunContext { project_id: active_id, workspace: workspace.clone() });
        Ok(workspace)
    }

    /// 実行コンテキストのプロセスがすべて止まっていれば、解放する。
    pub fn release_if_idle(&self) {
        let _gate = self.lock_gate();
        self.release_if_idle_locked();
    }

    /// `release_if_idle` の本体。呼び出し側が `lifecycle_gate` を持っていること。
    /// 一括起動・一括停止の最中は、プロセスがまだ無くても解放しない。
    fn release_if_idle_locked(&self) {
        if self.start_all_running.load(Ordering::Acquire) || self.stop_all_running.load(Ordering::Acquire) {
            return;
        }
        let mut run = self.lock_run();
        let Some(context) = run.as_ref() else { return };
        if !has_live_processes(&self.processes, &context.workspace) {
            *run = None;
            // プロセスが残っていなければ、Core を開始したアセットも終わっている。
            self.core_started.store(false, Ordering::Release);
        }
    }

    /// プロジェクトを変更してよいかを確かめ、よければ `lifecycle_gate` を返す。
    /// 変更はこれを持ったまま行う。実行中のプロジェクトは変更できない。
    pub fn ensure_mutable(&self, project_id: &str) -> Result<MutexGuard<'_, ()>, String> {
        let gate = self.lock_gate();
        self.release_if_idle_locked();
        if self.lock_run().as_ref().is_some_and(|run| run.project_id == project_id) {
            return Err("実行中のプロジェクトは変更できません。すべて停止してから変更してください。".to_owned());
        }
        Ok(gate)
    }

    /// アクティブなプロジェクトを切り替える。何かが実行中なら拒否する。
    /// 切り替えたら、前のプロジェクトの終了済みプロセスと通信の履歴を消す。プロジェクトを
    /// またいで同じメンバー id があると、前の履歴が新しいプロジェクトのものに見えるため。
    pub fn select_project(&self, project_id: &str) -> Result<(), String> {
        let _gate = self.lock_gate();
        self.release_if_idle_locked();
        if self.lock_run().is_some() || self.start_all_running.load(Ordering::Acquire) || self.stop_all_running.load(Ordering::Acquire) {
            return Err("実行中はプロジェクトを切り替えられません。すべて停止してから切り替えてください。".to_owned());
        }
        let mut store = self.lock_store()?;
        if !store.projects.contains_key(project_id) {
            return Err("そのプロジェクトはありません。".to_owned());
        }
        if store.active_project_id() == project_id {
            return Ok(());
        }
        // 先に保存し、成功してから切り替えと履歴の消去をまとめて行う。保存に失敗したときに
        // アクティブだけが変わり、前のプロジェクトの履歴が残るのを防ぐ。
        let mut app = store.app.clone();
        app.active_project_id = Some(project_id.to_owned());
        store.write_app(&app)?;
        store.app = app;
        drop(store);
        self.processes.forget_finished();
        self.monitor.reset();
        Ok(())
    }
}

/// 保存場所を指定する環境変数。試験で利用者の保存場所に触れずに動かすためと、
/// 持ち運び用の構成のために使う。
pub const DATA_DIR_VARIABLE: &str = "HAKONIWA_DESKTOP_MANAGER_DATA_DIR";

fn data_directory() -> PathBuf {
    match std::env::var_os(DATA_DIR_VARIABLE) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("HakoniwaDesktopManager"),
    }
}

/// 構成のアセットと Core コントローラーに、まだ終わっていないプロセスがあるか。
fn has_live_processes(processes: &ProcessManager, workspace: &Workspace) -> bool {
    let owners: Vec<&str> = workspace.assets.iter().map(|asset| asset.id.as_str()).chain(workspace.core_controller.iter().map(|controller| controller.id.as_str())).collect();
    processes
        .snapshots()
        .iter()
        // 状態が分からないプロセスは、生きているものとして扱う（解放も切り替えもしない）。
        .any(|snapshot| owners.contains(&snapshot.owner_id.as_str()) && matches!(snapshot.status, ProcessStatus::Starting | ProcessStatus::Running | ProcessStatus::Stopping | ProcessStatus::Unknown))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::tests::v1_demo;
    use crate::types::{ExecutionTarget, ProcessKind, ProgramSpec};
    use std::collections::BTreeMap;

    #[cfg(windows)]
    fn sleeper() -> ProgramSpec {
        ProgramSpec { program: "cmd".into(), args: vec!["/C".into(), "ping -n 60 127.0.0.1 > NUL".into()], cwd: None, env: BTreeMap::new(), target: ExecutionTarget::Native }
    }

    #[cfg(not(windows))]
    fn sleeper() -> ProgramSpec {
        ProgramSpec { program: "/bin/sh".into(), args: vec!["-c".into(), "exec sleep 60".into()], cwd: None, env: BTreeMap::new(), target: ExecutionTarget::Native }
    }

    /// gimbal の v1 を移行した保存場所に、空のプロジェクトをもう 1 つ足した状態。
    fn state(name: &str) -> (AppState, String, String) {
        let dir = std::env::temp_dir().join(format!("hdm-state-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(crate::store::V1_WORKSPACE_FILE), serde_json::to_vec(&v1_demo()).unwrap()).unwrap();
        let mut store = Store::open(&dir);
        let demo = store.active_project_id().to_owned();
        let other = crate::model::Project::empty("other".to_owned(), "Other".to_owned());
        store.save_project(&other).unwrap();
        store.projects.insert(other.id.clone(), other);
        (AppState::with_store(store, dir.clone(), dir.join("catalog.json")), demo, "other".to_owned())
    }

    fn start(state: &AppState, member: &str) {
        let _gate = state.lock_gate();
        let workspace = state.begin_run_locked().unwrap();
        let asset = workspace.assets.iter().find(|a| a.id == member).unwrap();
        state.processes.start(asset.id.clone(), asset.name.clone(), ProcessKind::Asset, sleeper()).unwrap();
    }

    /// 実行中は、そのプロジェクトの変更と切り替えを拒否し、止まれば解放して許す。
    #[test]
    fn running_blocks_edits_and_switching_until_idle() {
        let (state, demo, other) = state("block");
        start(&state, "rig-owner");
        assert_eq!(state.running_project_id().as_deref(), Some(demo.as_str()));
        assert!(state.ensure_mutable(&demo).is_err());
        assert!(state.ensure_mutable(&other).is_ok(), "実行していないプロジェクトは変更できる");
        assert!(state.select_project(&other).is_err());

        state.processes.stop_owner("rig-owner");
        assert!(state.ensure_mutable(&demo).is_ok(), "止まったら解放される");
        assert!(state.running_project_id().is_none());
        state.select_project(&other).unwrap();
        assert_eq!(state.lock_store().unwrap().active_project_id(), other);
    }

    /// 実行中の構成は凍結している。保存側が変わっても、停止は凍結した構成のプロセスを止める。
    #[test]
    fn the_run_keeps_a_frozen_workspace() {
        let (state, demo, _) = state("frozen");
        start(&state, "pilot");
        // 拒否をすり抜けて保存側を書き換えた場合でも（例えば別経路の不具合）、実行側は変わらない。
        state.lock_store().unwrap().projects.get_mut(&demo).unwrap().members.retain(|m| m.id != "pilot");
        let frozen = state.run_workspace().unwrap();
        assert!(frozen.assets.iter().any(|a| a.id == "pilot"));
        for asset in frozen.assets.iter().rev() {
            state.processes.stop_owner(&asset.id);
        }
        state.release_if_idle();
        assert!(state.running_project_id().is_none());
    }

    /// 別のプロジェクトの実行コンテキストがあるとき、アクティブなプロジェクトで起動しようとすると拒否する。
    #[test]
    fn a_run_of_another_project_blocks_starting() {
        let (state, _, other) = state("other-run");
        start(&state, "rig-owner");
        state.lock_store().unwrap().app.active_project_id = Some(other);
        let _gate = state.lock_gate();
        assert!(state.begin_run_locked().is_err());
        drop(_gate);
        state.processes.stop_owner("rig-owner");
    }

    /// 一括起動・一括停止の最中は、プロセスがまだ無くても解放しない。
    #[test]
    fn a_run_is_kept_while_a_bulk_operation_is_in_progress() {
        let (state, demo, _) = state("bulk");
        {
            let _gate = state.lock_gate();
            state.begin_run_locked().unwrap();
        }
        state.start_all_running.store(true, Ordering::Release);
        state.release_if_idle();
        assert_eq!(state.running_project_id().as_deref(), Some(demo.as_str()));
        assert!(state.ensure_mutable(&demo).is_err());
        state.start_all_running.store(false, Ordering::Release);
        state.release_if_idle();
        assert!(state.running_project_id().is_none());
    }

    /// 一括起動・一括停止の最中は、実行コンテキストがなくても切り替えを拒否する。
    #[test]
    fn switching_is_refused_during_a_bulk_operation() {
        let (state, _, other) = state("bulk-switch");
        state.stop_all_running.store(true, Ordering::Release);
        assert!(state.select_project(&other).is_err());
        state.stop_all_running.store(false, Ordering::Release);
        state.select_project(&other).unwrap();
    }

    /// 切り替えの保存に失敗したら、アクティブも履歴も変えない。
    #[test]
    fn a_failed_switch_changes_nothing() {
        let (state, demo, other) = state("switch-fail");
        start(&state, "rig-owner");
        state.processes.stop_owner("rig-owner");
        // app.json をディレクトリにして、置き換えを失敗させる。
        let app = state.data_directory.join("app.json");
        fs::remove_file(&app).unwrap();
        fs::create_dir(&app).unwrap();
        assert!(state.select_project(&other).is_err());
        assert_eq!(state.lock_store().unwrap().active_project_id(), demo);
        assert_eq!(state.processes.snapshots().len(), 1, "履歴は消さない");
        fs::remove_dir(&app).unwrap();
        state.select_project(&other).unwrap();
        assert_eq!(state.lock_store().unwrap().active_project_id(), other);
        assert!(state.processes.snapshots().is_empty());
    }

    /// 切り替えると、前のプロジェクトの終了済みプロセスの記録を消す。
    #[test]
    fn switching_forgets_finished_processes() {
        let (state, _, other) = state("forget");
        start(&state, "rig-owner");
        state.processes.stop_owner("rig-owner");
        assert_eq!(state.processes.snapshots().len(), 1);
        state.select_project(&other).unwrap();
        assert!(state.processes.snapshots().is_empty());
    }
}
