import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ConnectionSnapshot, WorkspaceSnapshot } from "./types";
import { selectAssetProcess, selectCoreProcess } from "./selectors";

type AlertSeverity = "error" | "disconnected" | "recovered";

type TopologyIssue = {
  key: string;
  severity: Exclude<AlertSeverity, "recovered">;
  title: string;
  detail: string;
  connectionId?: string;
};

type AlertToast = Omit<TopologyIssue, "severity"> & {
  id: string;
  severity: AlertSeverity;
};

const TOAST_LIFETIME_MS = 8_000;
const MAX_VISIBLE_ALERTS = 4;

function connectionLabel(connection: ConnectionSnapshot) {
  return connection.definition.label || `${connection.definition.source} → ${connection.definition.destination}`;
}

function collectIssues(snapshot: WorkspaceSnapshot): TopologyIssue[] {
  // 終了したプロセスは管理表に残り続けるので、履歴全体を見ると再起動しても
  // 障害が消えず、復旧トーストが永久に出ない。所有者ごとの代表だけを見る。
  const representatives = snapshot.workspace.assets
    .map((asset) => selectAssetProcess(snapshot.processes, asset.id))
    .concat(selectCoreProcess(snapshot.processes))
    .filter((process): process is NonNullable<typeof process> => Boolean(process));

  const processIssues: TopologyIssue[] = representatives
    .filter((process) => process.status === "failed")
    .map((process) => ({
      key: `process:${process.owner_id}`,
      severity: "error",
      title: `${process.owner_name} でエラーが発生`,
      detail: process.stderr_tail[process.stderr_tail.length - 1] || "プロセスが異常終了しました。",
    }));

  const connectionIssues: TopologyIssue[] = snapshot.connections
    .filter((connection) => connection.state === "disconnected")
    .map((connection) => ({
      key: `connection:${connection.definition.id}`,
      severity: connection.latest_error ? "error" : "disconnected",
      title: connection.latest_error ? `${connectionLabel(connection)} で通信エラー` : `${connectionLabel(connection)} が切断`,
      detail: connection.latest_error || "Hakoniwa-core との接続を確認できません。",
      connectionId: connection.definition.id,
    }));

  return [...processIssues, ...connectionIssues];
}

function severityText(severity: AlertSeverity) {
  if (severity === "error") return "エラー";
  if (severity === "disconnected") return "切断";
  return "復旧";
}

function severityIcon(severity: AlertSeverity) {
  if (severity === "error") return "!";
  if (severity === "disconnected") return "×";
  return "✓";
}

/**
 * Renders short-lived, actionable alerts only when an issue begins, changes
 * severity, or recovers. Existing issues are intentionally not announced at
 * initial load to avoid an alert storm when opening a workspace.
 */
export const TopologyAlertOverlay = memo(function TopologyAlertOverlay({
  snapshot,
  onSelectConnection,
}: {
  snapshot: WorkspaceSnapshot;
  onSelectConnection: (connectionId: string) => void;
}) {
  const issues = useMemo(() => collectIssues(snapshot), [snapshot]);
  const previousIssuesRef = useRef(new Map<string, TopologyIssue>());
  const initializedRef = useRef(false);
  const timerIdsRef = useRef(new Map<string, number>());
  const [toasts, setToasts] = useState<AlertToast[]>([]);
  const [liveMessage, setLiveMessage] = useState("");

  const dismiss = useCallback((id: string) => {
    const timer = timerIdsRef.current.get(id);
    if (timer !== undefined) {
      window.clearTimeout(timer);
      timerIdsRef.current.delete(id);
    }
    setToasts((current) => current.filter((toast) => toast.id !== id));
  }, []);

  const pushToast = useCallback((toast: AlertToast) => {
    setToasts((current) => [toast, ...current.filter((item) => item.key !== toast.key)].slice(0, MAX_VISIBLE_ALERTS));
    setLiveMessage(`${severityText(toast.severity)}: ${toast.title}`);

    const timer = window.setTimeout(() => dismiss(toast.id), TOAST_LIFETIME_MS);
    timerIdsRef.current.set(toast.id, timer);
  }, [dismiss]);

  useEffect(() => {
    const currentIssues = new Map(issues.map((issue) => [issue.key, issue]));
    const previousIssues = previousIssuesRef.current;

    if (!initializedRef.current) {
      previousIssuesRef.current = currentIssues;
      initializedRef.current = true;
      return;
    }

    currentIssues.forEach((issue, key) => {
      const previous = previousIssues.get(key);
      if (!previous || previous.severity !== issue.severity || previous.detail !== issue.detail) {
        pushToast({ ...issue, id: `${key}:${Date.now()}` });
      }
    });

    previousIssues.forEach((issue, key) => {
      if (!currentIssues.has(key)) {
        pushToast({
          id: `${key}:recovered:${Date.now()}`,
          key,
          severity: "recovered",
          title: `${issue.title.replace(/（.*?）|でエラーが発生|が切断|で通信エラー/g, "").trim()} が復旧`,
          detail: "最新の状態では正常な接続またはプロセス状態を確認しています。",
          connectionId: issue.connectionId,
        });
      }
    });

    previousIssuesRef.current = currentIssues;
  }, [issues, pushToast]);

  useEffect(() => () => {
    timerIdsRef.current.forEach((timer) => window.clearTimeout(timer));
    timerIdsRef.current.clear();
  }, []);

  if (toasts.length === 0) {
    return <p className="topology-alert-live" role="status" aria-live="polite">{liveMessage}</p>;
  }

  return (
    <>
      <p className="topology-alert-live" role="status" aria-live="assertive">{liveMessage}</p>
      <aside className="topology-alert-rail" aria-label="通信およびプロセスのアラート">
        {toasts.map((toast) => (
          <article key={toast.id} className={`topology-alert-card alert-${toast.severity}`}>
            <span className="topology-alert-icon" aria-hidden="true">{severityIcon(toast.severity)}</span>
            <div className="topology-alert-content">
              <span className="topology-alert-severity">{severityText(toast.severity)}</span>
              <strong>{toast.title}</strong>
              <p title={toast.detail}>{toast.detail}</p>
              {toast.connectionId && (
                <button type="button" className="topology-alert-action" onClick={() => onSelectConnection(toast.connectionId!)}>
                  接続の詳細を見る
                </button>
              )}
            </div>
            <button type="button" className="topology-alert-dismiss" onClick={() => dismiss(toast.id)} aria-label={`${toast.title} を閉じる`}>×</button>
          </article>
        ))}
      </aside>
    </>
  );
});
