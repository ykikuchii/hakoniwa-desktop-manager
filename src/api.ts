import { invoke } from "@tauri-apps/api/core";
import type {
  AssetDefinition,
  ConnectionDefinition,
  CoreCatalog,
  CoreInstallResult,
  ImportPreview,
  LifecycleCommandResult,
  ProcessSnapshot,
  Workspace,
  WorkspaceSnapshot,
} from "./types";

export const api = {
  snapshot: () => invoke<WorkspaceSnapshot>("get_snapshot"),
  saveWorkspace: (workspace: Workspace) => invoke<Workspace>("save_workspace", { workspace }),
  createAsset: (asset: AssetDefinition) => invoke<Workspace>("create_asset", { asset }),
  updateAsset: (asset: AssetDefinition) => invoke<Workspace>("update_asset", { asset }),
  deleteAsset: (assetId: string) => invoke<Workspace>("delete_asset", { assetId }),
  startAsset: (assetId: string) => invoke<ProcessSnapshot>("start_asset", { assetId }),
  stopAsset: (assetId: string) => invoke<ProcessSnapshot[]>("stop_asset", { assetId }),
  startCore: () => invoke<ProcessSnapshot>("start_core_controller"),
  stopCore: () => invoke<ProcessSnapshot[]>("stop_core_controller"),
  runLifecycle: (command: "start" | "stop" | "reset") => invoke<LifecycleCommandResult>("run_lifecycle_command", { command }),
  startAll: () => invoke<ProcessSnapshot[]>("start_all"),
  stopAll: () => invoke<ProcessSnapshot[]>("stop_all"),
  inspectDirectory: (path: string) => invoke<ImportPreview>("inspect_business_pack_directory", { path }),
  applyPreview: (preview: ImportPreview) => invoke<Workspace>("apply_import_preview", { preview }),
  catalog: () => invoke<CoreCatalog>("get_core_catalog"),
  saveCatalog: (catalog: CoreCatalog) => invoke<CoreCatalog>("save_core_catalog", { catalog }),
  installCore: (version: string) => invoke<CoreInstallResult>("install_approved_core", { version }),
  ingestBridgeLine: (connectionId: string, line: string) => invoke<void>("ingest_bridge_monitor_line", { connectionId, line }),
  recordHeartbeat: (connectionId: string, message: string) => invoke<void>("record_manual_communication_event", { connectionId, message }),
};

export function newAsset(): AssetDefinition {
  return {
    id: crypto.randomUUID(),
    name: "新しいアセット",
    role: "other",
    command: { program: "", args: [], cwd: null, env: {}, target: "native" },
    depends_on: ["core"],
    activation_timing: "manual",
    config_files: [],
    enabled: true,
  };
}

export function newConnection(): ConnectionDefinition {
  return {
    id: crypto.randomUUID(),
    source: "source",
    destination: "destination",
    label: "手動接続",
    transport: "unknown",
    pdu_names: [],
    endpoint_config: null,
    details: {},
    source_asset_id: null,
    destination_asset_id: null,
    owner_asset_id: null,
  };
}
