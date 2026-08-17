export type HostPlatform = "windows" | "macos" | "linux" | "wsl";
export type CpuArchitecture = "x64" | "arm64" | { other: string };
export type ExecutionTarget = "native" | { wsl: { distribution: string } };
export type AssetRole = "simulator" | "controller" | "visualizer" | "bridge" | "service" | "external_client" | "monitor" | "other";
export type ActivationTiming = "before_start" | "after_start" | "manual";
export type ProcessStatus = "starting" | "running" | "exited" | "failed" | "stopping" | "unknown";
export type ProcessKind = "core_controller" | "asset" | "lifecycle_command";
export type TransportKind = "shared_memory" | "tcp" | "udp" | "websocket" | "zenoh" | "mqtt" | "rpc" | "storage" | "unknown";
export type ConnectionState = "connected" | "idle" | "disconnected" | "unknown";
export type ObservationSource = "bridge_monitor" | "endpoint_log" | "core_monitor" | "config_import" | "manual";

export interface ProgramSpec {
  program: string;
  args: string[];
  cwd?: string | null;
  env: Record<string, string>;
  target: ExecutionTarget;
}

export interface AssetDefinition {
  id: string;
  name: string;
  role: AssetRole;
  command: ProgramSpec;
  depends_on: string[];
  activation_timing: ActivationTiming;
  config_files: string[];
  enabled: boolean;
}

export interface CoreController {
  id: string;
  name: string;
  command: ProgramSpec;
  readiness: { kind: "manual" } | { kind: "log_contains"; text: string } | { kind: "tcp_port"; host: string; port: number };
}

export interface InstalledCoreSelection {
  version: string;
  install_directory: string;
  hako_cmd_path: string;
  verified_sha256: string;
  installed_at: string;
}

export interface Workspace {
  schema_version: number;
  id: string;
  name: string;
  source_directory?: string | null;
  core_release?: InstalledCoreSelection | null;
  core_controller?: CoreController | null;
  assets: AssetDefinition[];
  imported_connections: ConnectionDefinition[];
  last_opened_at?: string | null;
}

export interface ProcessSnapshot {
  id: string;
  owner_id: string;
  owner_name: string;
  kind: ProcessKind;
  pid?: number | null;
  status: ProcessStatus;
  started_at: string;
  ended_at?: string | null;
  exit_code?: number | null;
  restart_count: number;
  stdout_tail: string[];
  stderr_tail: string[];
  target: ExecutionTarget;
}

export interface ConnectionDefinition {
  id: string;
  source: string;
  destination: string;
  label: string;
  transport: TransportKind;
  pdu_names: string[];
  endpoint_config?: string | null;
  details: Record<string, string>;
  /** Rust の linking がアセットへ解決した結果。相手がアセットでなければ null。 */
  source_asset_id?: string | null;
  destination_asset_id?: string | null;
  /** この接続を観測するアセット。Bridge 由来では端点ではなく Bridge 自身。 */
  owner_asset_id?: string | null;
}

export interface ConnectionSnapshot {
  definition: ConnectionDefinition;
  state: ConnectionState;
  last_activity_at?: string | null;
  messages_sent: number;
  messages_received: number;
  bytes_sent: number;
  bytes_received: number;
  latest_error?: string | null;
  observation_source: ObservationSource;
}

export interface CommunicationEvent {
  id: string;
  connection_id: string;
  observed_at: string;
  direction: "sent" | "received" | "bidirectional" | "lifecycle";
  event_type: "connected" | "disconnected" | "message" | "heartbeat" | "error" | "configuration";
  pdu_name?: string | null;
  byte_count?: number | null;
  message: string;
  source: ObservationSource;
}

export interface WorkspaceSnapshot {
  workspace: Workspace;
  platform: HostPlatform;
  architecture: CpuArchitecture;
  processes: ProcessSnapshot[];
  connections: ConnectionSnapshot[];
  recent_events: CommunicationEvent[];
}

export interface ImportPreview {
  source_directory: string;
  discovered_files: string[];
  assets: AssetDefinition[];
  connections: ConnectionDefinition[];
  warnings: string[];
}

export interface CoreArtifact {
  platform: HostPlatform;
  architecture: CpuArchitecture;
  url: string;
  sha256: string;
  archive_format: "zip" | "file";
  hako_cmd_relative_path: string;
  install_root?: string | null;
  provenance: { repository: string; release_tag: string; build_workflow: string };
}

export interface CoreRelease { version: string; source_revision: string; artifacts: CoreArtifact[] }
export interface CoreCatalog { schema_version: number; component: string; publisher: string; releases: CoreRelease[] }
export interface CoreInstallResult { selection: InstalledCoreSelection; artifact_url: string; artifact_sha256: string }
export interface LifecycleCommandResult { command: string; status: ProcessStatus; stdout: string; stderr: string }
