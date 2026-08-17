use crate::types::{ExecutionTarget, ProcessKind, ProcessSnapshot, ProcessStatus, ProgramSpec};
use chrono::Utc;
use std::{
    collections::BTreeMap,
    io::BufRead,
    io::BufReader,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, MutexGuard},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use thiserror::Error;
use uuid::Uuid;

const LOG_TAIL_LIMIT: usize = 500;
/// 1行あたりに保持する最大バイト数。改行を出さない子プロセスがメモリを食い潰すのを防ぐ。
const LOG_LINE_BYTE_LIMIT: usize = 8 * 1024;
/// 停止時にログ読取スレッドの終了を待つ上限。孫プロセスがパイプを握ったままでも固まらないようにする。
const READER_DRAIN_BUDGET: Duration = Duration::from_millis(500);

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("{0}")]
    Validation(String),
    #[error("コマンドを起動できませんでした: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("WSL実行はWindowsホストからのみ利用できます。")]
    WslUnsupported,
    #[error("管理対象のプロセスが見つかりません: {0}")]
    NotFound(String),
    #[error("プロセスを停止できませんでした: {0}")]
    Stop(String),
}

/// `stop_owner`の結果。停止できたプロセスと、停止に失敗したプロセスを取りこぼさず両方返す。
#[derive(Debug, Default)]
pub struct StopReport {
    pub stopped: Vec<ProcessSnapshot>,
    pub failures: Vec<String>,
}

impl StopReport {
    pub fn is_empty(&self) -> bool {
        self.stopped.is_empty() && self.failures.is_empty()
    }
}

struct ManagedProcess {
    child: Child,
    snapshot: ProcessSnapshot,
    stdout_tail: Arc<Mutex<Vec<String>>>,
    stderr_tail: Arc<Mutex<Vec<String>>>,
    readers: Vec<JoinHandle<()>>,
    /// 利用者の停止操作で終了させたかどうか。シグナル終了を異常終了と誤表示しないために使う。
    stop_requested: bool,
    /// スナップショットのログ末尾は毎回上書きされるため、管理側の警告は別に保持して後から足す。
    diagnostics: Vec<String>,
}

pub struct ProcessManager {
    processes: Mutex<BTreeMap<String, ManagedProcess>>,
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: Mutex::new(BTreeMap::new()),
        }
    }

    /// ロック保持中のpanicでプロセス管理APIが全滅しないよう、poisoningからは状態を回収して継続する。
    fn lock_processes(&self) -> MutexGuard<'_, BTreeMap<String, ManagedProcess>> {
        self.processes.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn start(
        &self,
        owner_id: String,
        owner_name: String,
        kind: ProcessKind,
        spec: ProgramSpec,
    ) -> Result<ProcessSnapshot, ProcessError> {
        spec.validate().map_err(ProcessError::Validation)?;
        let mut command = command_from_spec(&spec)?;
        command.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
        let mut child = command.spawn()?;
        let stdout_tail = Arc::new(Mutex::new(Vec::new()));
        let stderr_tail = Arc::new(Mutex::new(Vec::new()));
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(spawn_log_reader(stdout, Arc::clone(&stdout_tail)));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(spawn_log_reader(stderr, Arc::clone(&stderr_tail)));
        }
        let id = Uuid::new_v4().to_string();
        let snapshot = ProcessSnapshot {
            id: id.clone(),
            owner_id,
            owner_name,
            kind,
            pid: Some(child.id()),
            status: ProcessStatus::Starting,
            started_at: Utc::now(),
            ended_at: None,
            exit_code: None,
            restart_count: 0,
            stdout_tail: Vec::new(),
            stderr_tail: Vec::new(),
            target: spec.target,
        };
        let managed = ManagedProcess {
            child,
            snapshot,
            stdout_tail,
            stderr_tail,
            readers,
            stop_requested: false,
            diagnostics: Vec::new(),
        };
        {
            let mut processes = self.lock_processes();
            processes.insert(id.clone(), managed);
        }
        self.snapshot(&id)
    }

    pub fn snapshot(&self, process_id: &str) -> Result<ProcessSnapshot, ProcessError> {
        let mut processes = self.lock_processes();
        let managed = processes
            .get_mut(process_id)
            .ok_or_else(|| ProcessError::NotFound(process_id.to_owned()))?;
        refresh(managed);
        Ok(managed.snapshot.clone())
    }

    pub fn snapshots(&self) -> Vec<ProcessSnapshot> {
        let mut processes = self.lock_processes();
        processes.values_mut().map(|managed| {
            refresh(managed);
            managed.snapshot.clone()
        }).collect()
    }

    pub fn stop(&self, process_id: &str) -> Result<ProcessSnapshot, ProcessError> {
        let readers = {
            let mut processes = self.lock_processes();
            let managed = processes
                .get_mut(process_id)
                .ok_or_else(|| ProcessError::NotFound(process_id.to_owned()))?;
            refresh(managed);
            if !matches!(managed.snapshot.status, ProcessStatus::Running | ProcessStatus::Starting) {
                return Ok(managed.snapshot.clone());
            }
            managed.snapshot.status = ProcessStatus::Stopping;
            managed.stop_requested = true;
            if let Err(error) = managed.child.kill() {
                // kill と自然終了が競合した場合は既に終了しているだけなので、回収へ進む。
                if !matches!(managed.child.try_wait(), Ok(Some(_))) {
                    managed.snapshot.status = ProcessStatus::Unknown;
                    managed.diagnostics.push(format!("停止操作に失敗しました: {error}"));
                    refresh(managed);
                    return Err(ProcessError::Stop(error.to_string()));
                }
            }
            match managed.child.wait() {
                Ok(status) => finalize(managed, status),
                Err(error) => {
                    // 終了状態を確定できない。Stoppingのまま固定せず、再確認できる状態へ戻す。
                    managed.snapshot.status = ProcessStatus::Unknown;
                    managed.diagnostics.push(format!("終了状態を確認できませんでした: {error}"));
                    refresh(managed);
                    return Err(ProcessError::Stop(error.to_string()));
                }
            }
            std::mem::take(&mut managed.readers)
        };
        // ロックを手放してから、終了直前の出力を読み切るのを待つ。
        drain_readers(readers, READER_DRAIN_BUDGET);
        let mut processes = self.lock_processes();
        let managed = processes
            .get_mut(process_id)
            .ok_or_else(|| ProcessError::NotFound(process_id.to_owned()))?;
        refresh(managed);
        Ok(managed.snapshot.clone())
    }

    pub fn stop_owner(&self, owner_id: &str) -> StopReport {
        let process_ids: Vec<String> = self.lock_processes()
            .iter()
            .filter_map(|(id, managed)| (managed.snapshot.owner_id == owner_id).then(|| id.clone()))
            .collect();
        let mut report = StopReport::default();
        for id in process_ids {
            match self.stop(&id) {
                Ok(snapshot) => report.stopped.push(snapshot),
                Err(error) => report.failures.push(format!("{id}: {error}")),
            }
        }
        report
    }
}

pub fn run_oneshot(spec: &ProgramSpec) -> Result<(i32, String, String), ProcessError> {
    spec.validate().map_err(ProcessError::Validation)?;
    let output = command_from_spec(spec)?.output()?;
    Ok((
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

fn command_from_spec(spec: &ProgramSpec) -> Result<Command, ProcessError> {
    match &spec.target {
        ExecutionTarget::Native => {
            let mut command = Command::new(&spec.program);
            command.args(&spec.args).envs(&spec.env);
            if let Some(cwd) = &spec.cwd {
                command.current_dir(cwd);
            }
            Ok(command)
        }
        ExecutionTarget::Wsl { distribution } => {
            if std::env::consts::OS != "windows" {
                return Err(ProcessError::WslUnsupported);
            }
            if distribution.trim().is_empty() {
                return Err(ProcessError::Validation("WSLディストリビューション名を指定してください。".to_owned()));
            }
            let mut command = Command::new("wsl.exe");
            command.arg("-d").arg(distribution);
            if let Some(cwd) = &spec.cwd {
                command.arg("--cd").arg(cwd);
            }
            command.arg("--").arg("env");
            for (key, value) in &spec.env {
                command.arg(format!("{key}={value}"));
            }
            command.arg(&spec.program).args(&spec.args);
            Ok(command)
        }
    }
}

fn finalize(managed: &mut ManagedProcess, status: std::process::ExitStatus) {
    // 利用者の停止操作による終了は、シグナル終了でも異常終了として扱わない。
    managed.snapshot.status = if status.success() || managed.stop_requested {
        ProcessStatus::Exited
    } else {
        ProcessStatus::Failed
    };
    managed.snapshot.exit_code = status.code();
    managed.snapshot.ended_at = Some(Utc::now());
}

fn drain_readers(readers: Vec<JoinHandle<()>>, budget: Duration) {
    let deadline = Instant::now() + budget;
    for handle in readers {
        while !handle.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if handle.is_finished() {
            let _ = handle.join();
        }
        // 期限内に終わらない場合はパイプを孫プロセスが握っている。切り離して停止処理を進める。
    }
}

fn lock_tail(tail: &Mutex<Vec<String>>) -> MutexGuard<'_, Vec<String>> {
    tail.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn spawn_log_reader<R: std::io::Read + Send + 'static>(reader: R, output: Arc<Mutex<Vec<String>>>) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            match read_capped_line(&mut reader) {
                Some(line) => {
                    let mut tail = lock_tail(&output);
                    tail.push(line);
                    if tail.len() > LOG_TAIL_LIMIT {
                        let keep_from = tail.len() - LOG_TAIL_LIMIT;
                        tail.drain(0..keep_from);
                    }
                }
                None => break,
            }
        }
    })
}

/// 改行までを1行として読むが、保持するのは`LOG_LINE_BYTE_LIMIT`まで。
/// 超過分は読み捨てるので、改行を出さない子プロセスでもメモリは伸びない。
fn read_capped_line<R: std::io::Read>(reader: &mut BufReader<R>) -> Option<String> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut saw_input = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(_) => break,
        };
        if available.is_empty() {
            break;
        }
        saw_input = true;
        match available.iter().position(|byte| *byte == b'\n') {
            Some(index) => {
                append_capped(&mut bytes, &available[..index], &mut truncated);
                reader.consume(index + 1);
                let line = finish_line(bytes, truncated);
                return Some(line);
            }
            None => {
                let length = available.len();
                let chunk: Vec<u8> = available.to_vec();
                append_capped(&mut bytes, &chunk, &mut truncated);
                reader.consume(length);
            }
        }
    }
    if saw_input && !bytes.is_empty() {
        return Some(finish_line(bytes, truncated));
    }
    None
}

fn append_capped(bytes: &mut Vec<u8>, chunk: &[u8], truncated: &mut bool) {
    let room = LOG_LINE_BYTE_LIMIT.saturating_sub(bytes.len());
    if room == 0 {
        *truncated = true;
        return;
    }
    if chunk.len() > room {
        bytes.extend_from_slice(&chunk[..room]);
        *truncated = true;
    } else {
        bytes.extend_from_slice(chunk);
    }
}

fn finish_line(mut bytes: Vec<u8>, truncated: bool) -> String {
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    let mut line = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        line.push_str(" …（1行が長すぎるため切り詰めました）");
    }
    line
}

fn refresh(managed: &mut ManagedProcess) {
    if matches!(managed.snapshot.status, ProcessStatus::Starting | ProcessStatus::Running) {
        match managed.child.try_wait() {
            Ok(Some(status)) => finalize(managed, status),
            Ok(None) => managed.snapshot.status = ProcessStatus::Running,
            Err(error) => {
                managed.snapshot.status = ProcessStatus::Unknown;
                managed.diagnostics.push(format!("状態確認エラー: {error}"));
            }
        }
    }
    managed.snapshot.stdout_tail = lock_tail(&managed.stdout_tail).clone();
    managed.snapshot.stderr_tail = lock_tail(&managed.stderr_tail).clone();
    // ログ末尾は毎回置き換わるため、管理側の警告はここで足し直す。
    managed.snapshot.stderr_tail.extend(managed.diagnostics.iter().cloned());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ExecutionTarget, ProcessKind, ProgramSpec};
    use std::{collections::BTreeMap, io::Cursor, sync::mpsc, time::Duration};

    #[cfg(windows)]
    fn probe_spec() -> ProgramSpec {
        ProgramSpec { program: "cmd".into(), args: vec!["/C".into(), "echo hako".into()], cwd: None, env: BTreeMap::new(), target: ExecutionTarget::Native }
    }

    #[cfg(not(windows))]
    fn probe_spec() -> ProgramSpec {
        ProgramSpec { program: "/bin/sh".into(), args: vec!["-c".into(), "echo hako".into()], cwd: None, env: BTreeMap::new(), target: ExecutionTarget::Native }
    }

    #[cfg(windows)]
    fn sleeper_spec() -> ProgramSpec {
        ProgramSpec { program: "cmd".into(), args: vec!["/C".into(), "echo hako-start & ping -n 60 127.0.0.1 > NUL".into()], cwd: None, env: BTreeMap::new(), target: ExecutionTarget::Native }
    }

    #[cfg(not(windows))]
    fn sleeper_spec() -> ProgramSpec {
        ProgramSpec { program: "/bin/sh".into(), args: vec!["-c".into(), "echo hako-start; exec sleep 60".into()], cwd: None, env: BTreeMap::new(), target: ExecutionTarget::Native }
    }

    #[test]
    fn rejects_empty_program() {
        let spec = ProgramSpec { program: " ".into(), args: vec![], cwd: None, env: BTreeMap::new(), target: ExecutionTarget::Native };
        assert!(run_oneshot(&spec).is_err());
    }

    /// `start`が`processes`ロックを保持したまま`snapshot`を呼ぶと、
    /// 非再入ミューテックスの自己デッドロックでアプリ全体が固まる。
    /// 別スレッド＋タイムアウトで、戻ってこないことを失敗として検出する。
    #[test]
    fn start_returns_without_deadlock() {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let manager = ProcessManager::new();
            let started = manager.start("owner-1".to_owned(), "probe".to_owned(), ProcessKind::Asset, probe_spec());
            let observed = started.is_ok() && manager.snapshots().len() == 1;
            for snapshot in manager.snapshots() {
                let _ = manager.stop(&snapshot.id);
            }
            let _ = sender.send(observed);
        });
        match receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(observed) => assert!(observed, "起動したプロセスが管理下に登録されていません。"),
            Err(_) => panic!("ProcessManager::startが10秒以内に戻りませんでした（デッドロックの疑い）。"),
        }
    }

    /// 利用者の停止操作による終了を異常終了として表示しない。
    /// Unixではkillがシグナル終了になり`ExitStatus::success()`がfalseになる。
    #[test]
    fn requested_stop_is_not_reported_as_failure() {
        let manager = ProcessManager::new();
        let started = manager
            .start("owner-2".to_owned(), "sleeper".to_owned(), ProcessKind::Asset, sleeper_spec())
            .expect("起動できませんでした。");
        let stopped = manager.stop(&started.id).expect("停止できませんでした。");
        assert_eq!(stopped.status, ProcessStatus::Exited, "意図的な停止がFailedとして記録されています。");
        assert!(stopped.ended_at.is_some(), "終了時刻が記録されていません。");
    }

    /// 停止が返すスナップショットは、停止前に観測できていた出力を保持していること。
    /// `stop`がログ末尾を読み直さずに返すと、この行が消える。
    #[test]
    fn stop_snapshot_keeps_earlier_output() {
        let manager = ProcessManager::new();
        let started = manager
            .start("owner-3".to_owned(), "sleeper".to_owned(), ProcessKind::Asset, sleeper_spec())
            .expect("起動できませんでした。");
        // 子プロセスが実際に出力するまで待つ。ここで待たないと停止と出力が競合する。
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut observed = false;
        while std::time::Instant::now() < deadline {
            let snapshot = manager.snapshot(&started.id).expect("スナップショットを取得できませんでした。");
            if snapshot.stdout_tail.iter().any(|line| line.contains("hako-start")) {
                observed = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(observed, "子プロセスの出力を観測できませんでした。");
        let stopped = manager.stop(&started.id).expect("停止できませんでした。");
        assert!(
            stopped.stdout_tail.iter().any(|line| line.contains("hako-start")),
            "停止後のスナップショットから既存のログが失われています: {:?}",
            stopped.stdout_tail
        );
    }

    /// 停止済みプロセスへの再停止は、状態を壊さず現在のスナップショットを返す。
    #[test]
    fn stopping_twice_is_idempotent() {
        let manager = ProcessManager::new();
        let started = manager
            .start("owner-4".to_owned(), "sleeper".to_owned(), ProcessKind::Asset, sleeper_spec())
            .expect("起動できませんでした。");
        let first = manager.stop(&started.id).expect("停止できませんでした。");
        let second = manager.stop(&started.id).expect("2回目の停止でエラーになりました。");
        assert_eq!(first.status, second.status);
        assert_eq!(second.status, ProcessStatus::Exited);
    }

    /// 改行を出さない出力でも、保持する1行のバイト数は上限で頭打ちになる。
    #[test]
    fn caps_line_length_without_newline() {
        let payload = vec![b'a'; LOG_LINE_BYTE_LIMIT * 3];
        let mut reader = BufReader::new(Cursor::new(payload));
        let line = read_capped_line(&mut reader).expect("1行も読めませんでした。");
        assert!(
            line.len() < LOG_LINE_BYTE_LIMIT * 2,
            "1行の保持量が上限で抑えられていません: {}バイト",
            line.len()
        );
        assert!(line.contains("切り詰め"), "切り詰めが利用者に伝わりません。");
    }

    /// 存在しないownerの停止要求は、成功でも失敗でもない空の結果になる。
    #[test]
    fn stop_owner_reports_nothing_for_unknown_owner() {
        let manager = ProcessManager::new();
        let report = manager.stop_owner("missing-owner");
        assert!(report.is_empty());
    }
}
