use crate::types::{ExecutionTarget, ProcessKind, ProcessSnapshot, ProcessStatus, ProgramSpec};
use chrono::Utc;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};
use thiserror::Error;
use uuid::Uuid;

const LOG_TAIL_LIMIT: usize = 500;

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
}

struct ManagedProcess {
    child: Child,
    snapshot: ProcessSnapshot,
    stdout_tail: Arc<Mutex<Vec<String>>>,
    stderr_tail: Arc<Mutex<Vec<String>>>,
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
        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(stdout, Arc::clone(&stdout_tail));
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, Arc::clone(&stderr_tail));
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
        };
        let mut processes = self.processes.lock().expect("process state lock poisoned");
        processes.insert(id.clone(), managed);
        self.snapshot(&id)
    }

    pub fn snapshot(&self, process_id: &str) -> Result<ProcessSnapshot, ProcessError> {
        let mut processes = self.processes.lock().expect("process state lock poisoned");
        let managed = processes
            .get_mut(process_id)
            .ok_or_else(|| ProcessError::NotFound(process_id.to_owned()))?;
        refresh(managed);
        Ok(managed.snapshot.clone())
    }

    pub fn snapshots(&self) -> Vec<ProcessSnapshot> {
        let mut processes = self.processes.lock().expect("process state lock poisoned");
        processes.values_mut().map(|managed| {
            refresh(managed);
            managed.snapshot.clone()
        }).collect()
    }

    pub fn stop(&self, process_id: &str) -> Result<ProcessSnapshot, ProcessError> {
        let mut processes = self.processes.lock().expect("process state lock poisoned");
        let managed = processes
            .get_mut(process_id)
            .ok_or_else(|| ProcessError::NotFound(process_id.to_owned()))?;
        refresh(managed);
        if matches!(managed.snapshot.status, ProcessStatus::Running | ProcessStatus::Starting) {
            managed.snapshot.status = ProcessStatus::Stopping;
            managed.child.kill()?;
            let status = managed.child.wait()?;
            managed.snapshot.status = if status.success() { ProcessStatus::Exited } else { ProcessStatus::Failed };
            managed.snapshot.exit_code = status.code();
            managed.snapshot.ended_at = Some(Utc::now());
        }
        refresh(managed);
        Ok(managed.snapshot.clone())
    }

    pub fn stop_owner(&self, owner_id: &str) -> Vec<ProcessSnapshot> {
        let process_ids: Vec<String> = self.processes.lock().expect("process state lock poisoned")
            .iter()
            .filter_map(|(id, managed)| (managed.snapshot.owner_id == owner_id).then(|| id.clone()))
            .collect();
        process_ids.into_iter().filter_map(|id| self.stop(&id).ok()).collect()
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

fn spawn_log_reader<R: std::io::Read + Send + 'static>(reader: R, output: Arc<Mutex<Vec<String>>>) {
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines().map_while(Result::ok) {
            let mut tail = output.lock().expect("log tail lock poisoned");
            tail.push(line);
            if tail.len() > LOG_TAIL_LIMIT {
                let keep_from = tail.len() - LOG_TAIL_LIMIT;
                tail.drain(0..keep_from);
            }
        }
    });
}

fn refresh(managed: &mut ManagedProcess) {
    if matches!(managed.snapshot.status, ProcessStatus::Starting | ProcessStatus::Running) {
        match managed.child.try_wait() {
            Ok(Some(status)) => {
                managed.snapshot.status = if status.success() { ProcessStatus::Exited } else { ProcessStatus::Failed };
                managed.snapshot.exit_code = status.code();
                managed.snapshot.ended_at = Some(Utc::now());
            }
            Ok(None) => managed.snapshot.status = ProcessStatus::Running,
            Err(error) => {
                managed.snapshot.status = ProcessStatus::Unknown;
                managed.snapshot.stderr_tail.push(format!("状態確認エラー: {error}"));
            }
        }
    }
    managed.snapshot.stdout_tail = managed.stdout_tail.lock().expect("stdout lock poisoned").clone();
    managed.snapshot.stderr_tail = managed.stderr_tail.lock().expect("stderr lock poisoned").clone();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ExecutionTarget, ProgramSpec};
    use std::collections::BTreeMap;

    #[test]
    fn rejects_empty_program() {
        let spec = ProgramSpec { program: " ".into(), args: vec![], cwd: None, env: BTreeMap::new(), target: ExecutionTarget::Native };
        assert!(run_oneshot(&spec).is_err());
    }
}
