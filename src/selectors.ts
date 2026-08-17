import type { ConnectionSnapshot, ProcessSnapshot } from "./types";

/// ProcessManager は終了したプロセスも履歴として保持し続けるため、owner_id で
/// 素朴に引くと stop→start のあとに古い実体を掴む。表示側はこの規則で選ぶ。
export const ACTIVE_PROCESS_STATUSES = ["running", "starting", "stopping"];

export function findActiveProcess(processes: ProcessSnapshot[], ownerId: string): ProcessSnapshot | undefined {
  return processes.find((process) => process.owner_id === ownerId && ACTIVE_PROCESS_STATUSES.includes(process.status));
}

/**
 * このアセットを代表するプロセス。稼働中があればそれ、なければ最後に起動した実体。
 * 稼働中だけに絞ると、異常終了した直後のアセットが「未確認」になって
 * エラー表示が消えてしまうため、終了済みでも最新の1件は残す。
 */
export function pickRepresentativeProcess(processes: ProcessSnapshot[]): ProcessSnapshot | undefined {
  const active = processes.find((process) => ACTIVE_PROCESS_STATUSES.includes(process.status));
  if (active) return active;
  return processes.reduce<ProcessSnapshot | undefined>((latest, candidate) => (
    !latest || Date.parse(candidate.started_at) >= Date.parse(latest.started_at) ? candidate : latest
  ), undefined);
}

export function selectAssetProcess(processes: ProcessSnapshot[], ownerId: string): ProcessSnapshot | undefined {
  return pickRepresentativeProcess(processes.filter((process) => process.owner_id === ownerId));
}

/// Core コントローラーも同じ理由で、履歴の先頭ではなく代表を選ぶ。
export function selectCoreProcess(processes: ProcessSnapshot[]): ProcessSnapshot | undefined {
  return pickRepresentativeProcess(processes.filter((process) => process.kind === "core_controller"));
}

/// 接続とアセットの対応付けは Rust の linking が解決済み。ここでは結果を読むだけ。
export function connectionsForAsset(connections: ConnectionSnapshot[], assetId: string): ConnectionSnapshot[] {
  return connections.filter((connection) => (
    connection.definition.source_asset_id === assetId
    || connection.definition.destination_asset_id === assetId
    || connection.definition.owner_asset_id === assetId
  ));
}

/// 配列の先頭ではなく、実際に最後に通信した時刻。
export function latestActivity(connections: ConnectionSnapshot[]): string | null {
  return connections.reduce<string | null>((latest, connection) => {
    const value = connection.last_activity_at;
    if (!value) return latest;
    return !latest || Date.parse(value) > Date.parse(latest) ? value : latest;
  }, null);
}
