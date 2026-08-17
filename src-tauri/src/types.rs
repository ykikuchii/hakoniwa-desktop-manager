use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const WORKSPACE_SCHEMA_VERSION: u32 = 1;
pub const CATALOG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostPlatform {
    Windows,
    Macos,
    Linux,
    Wsl,
}

impl HostPlatform {
    pub fn current() -> Self {
        match std::env::consts::OS {
            "windows" => Self::Windows,
            "macos" => Self::Macos,
            _ => Self::Linux,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::Linux => "linux",
            Self::Wsl => "wsl",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CpuArchitecture {
    X64,
    Arm64,
    Other(String),
}

impl CpuArchitecture {
    pub fn current() -> Self {
        match std::env::consts::ARCH {
            "x86_64" | "amd64" => Self::X64,
            "aarch64" => Self::Arm64,
            other => Self::Other(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::X64 => "x64",
            Self::Arm64 => "arm64",
            Self::Other(value) => value,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTarget {
    Native,
    Wsl { distribution: String },
}

impl Default for ExecutionTarget {
    fn default() -> Self {
        Self::Native
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub target: ExecutionTarget,
}

impl ProgramSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.program.trim().is_empty() {
            return Err("実行ファイルまたはコマンドを指定してください。".to_owned());
        }
        if self.program.contains('\0') || self.args.iter().any(|arg| arg.contains('\0')) {
            return Err("NUL文字を含むコマンドは実行できません。".to_owned());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreController {
    pub id: String,
    pub name: String,
    pub command: ProgramSpec,
    #[serde(default)]
    pub readiness: ReadinessCheck,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReadinessCheck {
    Manual,
    LogContains { text: String },
    TcpPort { host: String, port: u16 },
}

impl Default for ReadinessCheck {
    fn default() -> Self {
        Self::Manual
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetDefinition {
    pub id: String,
    pub name: String,
    pub role: AssetRole,
    pub command: ProgramSpec,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub activation_timing: ActivationTiming,
    #[serde(default)]
    pub config_files: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssetRole {
    Simulator,
    Controller,
    Visualizer,
    Bridge,
    Service,
    ExternalClient,
    Monitor,
    Other,
}

impl Default for AssetRole {
    fn default() -> Self {
        Self::Other
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivationTiming {
    BeforeStart,
    AfterStart,
    Manual,
}

impl Default for ActivationTiming {
    fn default() -> Self {
        Self::Manual
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub source_directory: Option<String>,
    #[serde(default)]
    pub core_release: Option<InstalledCoreSelection>,
    #[serde(default)]
    pub core_controller: Option<CoreController>,
    #[serde(default)]
    pub assets: Vec<AssetDefinition>,
    #[serde(default)]
    pub imported_connections: Vec<ConnectionDefinition>,
    #[serde(default)]
    pub last_opened_at: Option<DateTime<Utc>>,
}

impl Workspace {
    pub fn empty(id: String, name: String) -> Self {
        Self {
            schema_version: WORKSPACE_SCHEMA_VERSION,
            id,
            name,
            source_directory: None,
            core_release: None,
            core_controller: None,
            assets: Vec::new(),
            imported_connections: Vec::new(),
            last_opened_at: Some(Utc::now()),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != WORKSPACE_SCHEMA_VERSION {
            return Err(format!(
                "このワークスペース形式（v{}）は未対応です。",
                self.schema_version
            ));
        }
        if self.name.trim().is_empty() {
            return Err("ワークスペース名を指定してください。".to_owned());
        }
        let mut ids = std::collections::BTreeSet::new();
        for asset in &self.assets {
            if asset.name.trim().is_empty() {
                return Err("アセット名を指定してください。".to_owned());
            }
            asset.command.validate()?;
            if !ids.insert(asset.id.clone()) {
                return Err("アセットIDが重複しています。".to_owned());
            }
        }
        for asset in &self.assets {
            for dependency in &asset.depends_on {
                if dependency != "core" && !ids.contains(dependency) {
                    return Err(format!(
                        "アセット「{}」は未登録の依存先「{}」を参照しています。",
                        asset.name, dependency
                    ));
                }
                if dependency == &asset.id {
                    return Err(format!("アセット「{}」が自分自身に依存しています。", asset.name));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledCoreSelection {
    pub version: String,
    pub install_directory: String,
    pub hako_cmd_path: String,
    pub verified_sha256: String,
    pub installed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreCatalog {
    pub schema_version: u32,
    pub component: String,
    pub publisher: String,
    pub releases: Vec<CoreRelease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreRelease {
    pub version: String,
    pub source_revision: String,
    #[serde(default)]
    pub artifacts: Vec<CoreArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreArtifact {
    pub platform: HostPlatform,
    pub architecture: CpuArchitecture,
    pub url: String,
    pub sha256: String,
    pub archive_format: ArchiveFormat,
    pub hako_cmd_relative_path: String,
    #[serde(default)]
    pub install_root: Option<String>,
    #[serde(default)]
    pub provenance: ArtifactProvenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    Zip,
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ArtifactProvenance {
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub release_tag: String,
    #[serde(default)]
    pub build_workflow: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSnapshot {
    pub id: String,
    pub owner_id: String,
    pub owner_name: String,
    pub kind: ProcessKind,
    pub pid: Option<u32>,
    pub status: ProcessStatus,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub restart_count: u32,
    pub stdout_tail: Vec<String>,
    pub stderr_tail: Vec<String>,
    pub target: ExecutionTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessKind {
    CoreController,
    Asset,
    LifecycleCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Starting,
    Running,
    Exited,
    Failed,
    Stopping,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionDefinition {
    pub id: String,
    pub source: String,
    pub destination: String,
    pub label: String,
    pub transport: TransportKind,
    #[serde(default)]
    pub pdu_names: Vec<String>,
    #[serde(default)]
    pub endpoint_config: Option<String>,
    #[serde(default)]
    pub details: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    SharedMemory,
    Tcp,
    Udp,
    Websocket,
    Zenoh,
    Mqtt,
    Rpc,
    Storage,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Connected,
    Idle,
    Disconnected,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunicationEvent {
    pub id: String,
    pub connection_id: String,
    pub observed_at: DateTime<Utc>,
    pub direction: EventDirection,
    pub event_type: CommunicationEventType,
    #[serde(default)]
    pub pdu_name: Option<String>,
    #[serde(default)]
    pub byte_count: Option<u64>,
    #[serde(default)]
    pub message: String,
    pub source: ObservationSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventDirection {
    Sent,
    Received,
    Bidirectional,
    Lifecycle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommunicationEventType {
    Connected,
    Disconnected,
    Message,
    Heartbeat,
    Error,
    Configuration,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    BridgeMonitor,
    EndpointLog,
    CoreMonitor,
    ConfigImport,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionSnapshot {
    pub definition: ConnectionDefinition,
    pub state: ConnectionState,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub messages_sent: u64,
    pub messages_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub latest_error: Option<String>,
    pub observation_source: ObservationSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub workspace: Workspace,
    pub platform: HostPlatform,
    pub architecture: CpuArchitecture,
    pub processes: Vec<ProcessSnapshot>,
    pub connections: Vec<ConnectionSnapshot>,
    pub recent_events: Vec<CommunicationEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportPreview {
    pub source_directory: String,
    pub discovered_files: Vec<String>,
    pub assets: Vec<AssetDefinition>,
    pub connections: Vec<ConnectionDefinition>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreInstallResult {
    pub selection: InstalledCoreSelection,
    pub artifact_url: String,
    pub artifact_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleCommandResult {
    pub command: String,
    pub status: ProcessStatus,
    pub stdout: String,
    pub stderr: String,
}
