import { useEffect, useMemo, useRef, useState } from "react";
import type { ConnectionSnapshot, PduFlow, ProcessSnapshot, ProjectCheck, WorkspaceSnapshot } from "./types";
import { TopologyAlertOverlay } from "./TopologyAlertOverlay";
import { connectionsForAsset, latestActivity, selectAssetProcess, selectCoreProcess } from "./selectors";

type VisualStatus = "running" | "starting" | "connected" | "disconnected" | "stopped" | "error";
type Position = { x: number; y: number };

type AssetNode = {
  id: string;
  name: string;
  role: string;
  status: VisualStatus;
  position: Position;
  process?: ProcessSnapshot;
  connections: ConnectionSnapshot[];
};

const statusLabels: Record<VisualStatus, string> = {
  running: "稼働中",
  starting: "起動中",
  connected: "接続中",
  disconnected: "切断",
  stopped: "停止中",
  error: "エラー",
};

function formatDate(value?: string | null) {
  if (!value) return "未取得";
  const date = new Date(value);
  return Number.isNaN(date.valueOf())
    ? value
    : new Intl.DateTimeFormat("ja-JP", { hour: "2-digit", minute: "2-digit", second: "2-digit" }).format(date);
}

function statusFor(process: ProcessSnapshot | undefined, connections: ConnectionSnapshot[]): VisualStatus {
  if (process?.status === "failed") return "error";
  if (process?.status === "starting") return "starting";
  if (process?.status === "running" && connections.some((connection) => connection.state === "connected")) return "connected";
  if (process?.status === "running") return "running";
  // 利用者が止めたものは、接続履歴が切断を示していても「停止」と表示する。
  if (process?.status === "exited") return "stopped";
  if (connections.some((connection) => connection.state === "disconnected")) return "disconnected";
  // 一括起動の対象外は「単独で起動するもの」で、止まっていても異常ではない。
  return "stopped";
}

function positionFor(index: number, total: number): Position {
  const innerRingCapacity = 6;
  const ring = Math.floor(index / innerRingCapacity);
  const start = ring * innerRingCapacity;
  const ringSize = Math.min(innerRingCapacity, total - start);
  const angle = ((index - start) / Math.max(ringSize, 1)) * Math.PI * 2 - Math.PI / 2 + (ring % 2 ? Math.PI / 6 : 0);
  const radius = ring === 0 ? 33 : 44 + (ring - 1) * 7;

  return {
    x: 50 + Math.cos(angle) * radius,
    y: 50 + Math.sin(angle) * radius,
  };
}

/**
 * 2 点（画素）を結ぶ曲線。中央の Core と重ならないよう、線の中ほどが中心から `clearance` 以上
 * 離れる側へ膨らませる。ラベルは曲線の中ほどに置く。
 */
// node はカード（114×74 px）の半分の大きさに、矢印の分の余白を足したもの。
function flowPath(from: Position, to: Position, center: Position, clearance: number, node = { halfWidth: 61, halfHeight: 41 }) {
  const mx = (from.x + to.x) / 2;
  const my = (from.y + to.y) / 2;
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const length = Math.hypot(dx, dy) || 1;
  let nx = -dy / length;
  let ny = dx / length;
  // 中心から離れる側へ曲げる。
  if ((mx - center.x) * nx + (my - center.y) * ny < 0) { nx = -nx; ny = -ny; }
  const distance = Math.hypot(mx - center.x, my - center.y);
  // 二次ベジェの中ほどは、直線の中点から制御点の方向へ半分だけ進んだ所。
  const bend = Math.max(length * 0.12, 2 * Math.max(0, clearance - distance));
  const cx = mx + nx * bend;
  const cy = my + ny * bend;
  // 両端はノードの中心ではなく、カード（長方形）の縁で止める（矢印がカードに隠れないように）。
  const trim = (point: Position) => {
    const ux = cx - point.x;
    const uy = cy - point.y;
    const scale = Math.min(ux ? node.halfWidth / Math.abs(ux) : Infinity, uy ? node.halfHeight / Math.abs(uy) : Infinity, 1);
    return { x: point.x + ux * scale, y: point.y + uy * scale };
  };
  const start = trim(from);
  const end = trim(to);
  return { d: `M ${start.x} ${start.y} Q ${cx} ${cy} ${end.x} ${end.y}`, label: { x: (mx + cx) / 2, y: (my + cy) / 2 } };
}

function flowKey(flow: PduFlow) {
  return `${flow.a}\u0000${flow.b}`;
}

const directionArrow: Record<PduFlow["channels"][number]["direction"], string> = { a_to_b: "→", b_to_a: "←", both: "⇄", shared: "―" };

function compactCount(value: number) {
  return new Intl.NumberFormat("ja-JP", { notation: "compact", maximumFractionDigits: 1 }).format(value);
}

export function TopologyPage({
  check,
  snapshot,
  selected,
  onSelect,
  onHeartbeat,
}: {
  check: ProjectCheck | null;
  snapshot: WorkspaceSnapshot;
  selected: ConnectionSnapshot | null;
  onSelect: (id: string) => void;
  onHeartbeat: (id: string) => void;
}) {
  const [hoveredId, setHoveredId] = useState<string | null>(null);
  const [activeNodeId, setActiveNodeId] = useState<string | null>(null);
  const [showFlows, setShowFlows] = useState(true);
  const [selectedFlowKey, setSelectedFlowKey] = useState<string | null>(null);
  // 流れの線は画素で描く（縦横比の違う枠に 100×100 の図を伸ばすと、矢印の形が崩れるため）。
  const stageRef = useRef<HTMLDivElement | null>(null);
  const [stageSize, setStageSize] = useState({ width: 0, height: 0 });
  useEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;
    const observer = new ResizeObserver(([entry]) => setStageSize({ width: entry.contentRect.width, height: entry.contentRect.height }));
    observer.observe(stage);
    return () => observer.disconnect();
  }, []);

  const coreProcess = useMemo(() => selectCoreProcess(snapshot.processes), [snapshot.processes]);
  const coreStatus: VisualStatus = coreProcess?.status === "failed"
    ? "error"
    : coreProcess?.status === "starting"
      ? "starting"
      : coreProcess?.status === "running"
        ? "running"
        : "stopped";

  const assets = useMemo<AssetNode[]>(() => snapshot.workspace.assets.map((asset, index) => {
    const process = selectAssetProcess(snapshot.processes, asset.id);
    const connections = connectionsForAsset(snapshot.connections, asset.id);

    return {
      id: asset.id,
      name: asset.name,
      role: asset.role,
      status: statusFor(process, connections),
      position: positionFor(index, snapshot.workspace.assets.length),
      process,
      connections,
    };
  }), [snapshot]);

  const positions = useMemo(() => new Map(assets.map((asset) => [asset.id, asset.position])), [assets]);
  // 線を引けるのは、両端がこの画面のアセットであるものだけ。
  const flows = useMemo(() => (check?.flows ?? []).filter((flow) => positions.has(flow.a) && positions.has(flow.b)), [check, positions]);
  const selectedFlow = flows.find((flow) => flowKey(flow) === selectedFlowKey) ?? null;
  // 選んでいた流れがなくなったら、選択を外す。
  useEffect(() => { if (selectedFlowKey && !selectedFlow) setSelectedFlowKey(null); }, [selectedFlowKey, selectedFlow]);
  const toPixels = (position: Position) => ({ x: (position.x / 100) * stageSize.width, y: (position.y / 100) * stageSize.height });
  const center = toPixels({ x: 50, y: 50 });
  const clearance = Math.min(stageSize.width, stageSize.height) * 0.16;
  const drawn = flows.map((flow) => {
    const directions = new Set(flow.channels.map((channel) => channel.direction));
    return {
      flow,
      key: flowKey(flow),
      path: flowPath(toPixels(positions.get(flow.a)!), toPixels(positions.get(flow.b)!), center, clearance),
      toB: directions.has("a_to_b") || directions.has("both"),
      toA: directions.has("b_to_a") || directions.has("both"),
    };
  });
  const nameOf = (id: string) => assets.find((asset) => asset.id === id)?.name ?? id;
  const selectedConnection = selected ?? null;
  const activeNode = assets.find((asset) => asset.id === activeNodeId) ?? null;
  const hoveredNode = assets.find((asset) => asset.id === hoveredId) ?? null;
  const detailNode = activeNode ?? hoveredNode;
  const connectedCount = snapshot.connections.filter((connection) => connection.state === "connected").length;
  const runningCount = snapshot.processes.filter((process) => process.status === "running").length;
  // 失敗は履歴の総数ではなくアセット単位で数える。終了済みプロセスは管理表に
  // 残り続けるため、総数で数えると再起動しても「注意」が減らない。
  const failedAssetCount = assets.filter((asset) => asset.status === "error").length
    + (coreStatus === "error" ? 1 : 0);
  const alertCount = failedAssetCount
    + snapshot.connections.filter((connection) => connection.state === "disconnected" && connection.latest_error).length;

  function activateNode(node: AssetNode) {
    setActiveNodeId(node.id);
    const preferred = node.connections.find((connection) => connection.state === "connected") ?? node.connections[0];
    if (preferred) onSelect(preferred.definition.id);
  }

  function activateConnection(connection: ConnectionSnapshot) {
    onSelect(connection.definition.id);
    const target = assets.find((asset) => (
      asset.id === connection.definition.source_asset_id
      || asset.id === connection.definition.destination_asset_id
      || asset.id === connection.definition.owner_asset_id
    ));
    setActiveNodeId(target?.id ?? null);
  }

  return (
    <section className="topology-page topology-experience">
      <TopologyAlertOverlay snapshot={snapshot} onSelectConnection={onSelect} />
      <header className="topology-header">
        <div>
          <p className="eyebrow">LIVE CONNECTION MAP</p>
          <h2>Hakoniwa 接続トポロジー</h2>
          <p className="muted">Hakoniwa-core と登録アセットの稼働・通信状態をリアルタイムに把握できます。</p>
        </div>
        <div className="topology-summary" aria-label="接続状況サマリー">
          <span className="topology-summary-item"><i className="summary-dot summary-dot-running" />稼働 <strong>{runningCount}</strong></span>
          <span className="topology-summary-item"><i className="summary-dot summary-dot-connected" />接続 <strong>{connectedCount}</strong></span>
          <span className="topology-summary-item"><i className="summary-dot summary-dot-alert" />注意 <strong>{alertCount}</strong></span>
        </div>
      </header>

      <div className="topology-layout">
        <div className="topology-stage" aria-label="Hakoniwa の接続関係図" ref={stageRef}>
          <div className="topology-stage-glow" aria-hidden="true" />
          <svg className="topology-network" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
            {assets.map((asset) => {
              const activeConnection = asset.connections.find((connection) => connection.state === "connected");
              const inactiveConnection = asset.connections[0];
              const connection = activeConnection ?? inactiveConnection;
              const isConnected = Boolean(activeConnection);
              const isSelected = selectedConnection?.definition.id === connection?.definition.id;

              return (
                <g key={`edge-${asset.id}`} className={`network-edge ${isConnected ? "is-connected" : ""} ${isSelected ? "is-selected" : ""}`}>
                  <line x1="50" y1="50" x2={asset.position.x} y2={asset.position.y} />
                  {isConnected && <line className="network-edge-flow" x1="50" y1="50" x2={asset.position.x} y2={asset.position.y} />}
                </g>
              );
            })}
          </svg>
          {showFlows && stageSize.width > 0 && (
            <svg className="pdu-flow-layer" viewBox={`0 0 ${stageSize.width} ${stageSize.height}`} aria-hidden="true">
              <defs>
                <marker id="pdu-flow-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" markerUnits="userSpaceOnUse" orient="auto-start-reverse">
                  <path d="M 0 0 L 10 5 L 0 10 z" className="pdu-flow-arrowhead" />
                </marker>
              </defs>
              {drawn.map(({ flow, key, path, toA, toB }) => (
                <path
                  key={key}
                  d={path.d}
                  className={`pdu-flow ${toA || toB ? "" : "is-shared"} ${selectedFlowKey === key ? "is-selected" : ""}`}
                  markerEnd={toB ? "url(#pdu-flow-arrow)" : undefined}
                  markerStart={toA ? "url(#pdu-flow-arrow)" : undefined}
                  data-pair={`${flow.a} ${flow.b}`}
                />
              ))}
            </svg>
          )}
          {showFlows && drawn.map(({ flow, key, path }) => (
            <button
              key={`label-${key}`}
              type="button"
              className={`pdu-flow-label ${selectedFlowKey === key ? "is-selected" : ""}`}
              style={{ left: `${path.label.x}px`, top: `${path.label.y}px` }}
              aria-label={`PDU: ${nameOf(flow.a)} と ${nameOf(flow.b)}（${flow.channels.length} チャネル）`}
              onClick={() => setSelectedFlowKey(selectedFlowKey === key ? null : key)}
            >
              {flow.channels.length}
            </button>
          ))}

          <button
            type="button"
            className={`topology-core-node status-${coreStatus}`}
            aria-label={`Hakoniwa-core: ${statusLabels[coreStatus]}`}
            onClick={() => setActiveNodeId(null)}
            onFocus={() => setHoveredId("core")}
            onBlur={() => setHoveredId(null)}
            onMouseEnter={() => setHoveredId("core")}
            onMouseLeave={() => setHoveredId(null)}
          >
            <span className="core-orbit core-orbit-one" />
            <span className="core-orbit core-orbit-two" />
            <span className="core-symbol">H</span>
            <span className="node-kicker">SYSTEM CORE</span>
            <strong>Hakoniwa-core</strong>
            <small>{statusLabels[coreStatus]}</small>
          </button>

          {assets.map((asset) => {
            const style = { left: `${asset.position.x}%`, top: `${asset.position.y}%` };
            const isActive = activeNodeId === asset.id || hoveredId === asset.id;
            return (
              <button
                key={asset.id}
                type="button"
                className={`topology-asset-node status-${asset.status} ${isActive ? "is-active" : ""}`}
                style={style}
                aria-pressed={activeNodeId === asset.id}
                aria-label={`${asset.name}: ${statusLabels[asset.status]}`}
                onClick={() => activateNode(asset)}
                onFocus={() => setHoveredId(asset.id)}
                onBlur={() => setHoveredId(null)}
                onMouseEnter={() => setHoveredId(asset.id)}
                onMouseLeave={() => setHoveredId(null)}
              >
                <span className="asset-status-icon" aria-hidden="true" />
                <span className="asset-node-name">{asset.name}</span>
                <span className="asset-node-role">{asset.role}</span>
              </button>
            );
          })}

          {hoveredId === "core" && (
            <div className="topology-tooltip topology-tooltip-core" role="tooltip">
              <strong>Hakoniwa-core</strong>
              <span>{statusLabels[coreStatus]} · PID {coreProcess?.pid ?? "未取得"}</span>
              <span>起動: {formatDate(coreProcess?.started_at)}</span>
            </div>
          )}
          {hoveredNode && (
            <div
              className="topology-tooltip"
              style={{ left: `${Math.min(Math.max(hoveredNode.position.x, 18), 82)}%`, top: `${Math.min(Math.max(hoveredNode.position.y - 16, 12), 78)}%` }}
              role="tooltip"
            >
              <strong>{hoveredNode.name}</strong>
              <span>{statusLabels[hoveredNode.status]} · {hoveredNode.role}</span>
              <span>接続 {hoveredNode.connections.filter((connection) => connection.state === "connected").length} 件 · 最終通信 {formatDate(latestActivity(hoveredNode.connections))}</span>
            </div>
          )}
        </div>

        <aside className="topology-inspector" aria-live="polite">
          <div className="topology-inspector-heading">
            <span className="inspector-icon" aria-hidden="true">{detailNode ? "◌" : "◎"}</span>
            <div>
              <p className="eyebrow">DETAIL INSPECTOR</p>
              <h3>{detailNode?.name ?? "Hakoniwa-core"}</h3>
            </div>
          </div>

          {detailNode ? (
            <>
              <div className="inspector-status-row">
                <span className={`status-badge status-${detailNode.status}`}>{statusLabels[detailNode.status]}</span>
                <span>{detailNode.role}</span>
              </div>
              <dl className="inspector-list">
                <div><dt>プロセス</dt><dd>{detailNode.process?.pid ? `PID ${detailNode.process.pid}` : "未起動"}</dd></div>
                <div><dt>起動時刻</dt><dd>{formatDate(detailNode.process?.started_at)}</dd></div>
                <div><dt>接続数</dt><dd>{detailNode.connections.length} 件（接続中 {detailNode.connections.filter((connection) => connection.state === "connected").length} 件）</dd></div>
                <div><dt>最終通信</dt><dd>{formatDate(latestActivity(detailNode.connections))}</dd></div>
              </dl>
              {detailNode.connections.length > 0 ? (
                <div className="inspector-connections">
                  <span className="inspector-label">接続の詳細</span>
                  {detailNode.connections.map((connection) => (
                    <button
                      className={`inspector-connection ${selectedConnection?.definition.id === connection.definition.id ? "is-selected" : ""}`}
                      key={connection.definition.id}
                      type="button"
                      onClick={() => activateConnection(connection)}
                    >
                      <span className={`connection-state-dot state-${connection.state}`} />
                      <span>{connection.definition.label || `${connection.definition.source} → ${connection.definition.destination}`}</span>
                      <small>{connection.state === "connected" ? `${compactCount(connection.messages_sent + connection.messages_received)} メッセージ` : "未接続"}</small>
                    </button>
                  ))}
                </div>
              ) : <p className="inspector-empty">このアセットに紐づく接続はまだ登録されていません。</p>}
            </>
          ) : (
            <>
              <div className="inspector-status-row">
                <span className={`status-badge status-${coreStatus}`}>{statusLabels[coreStatus]}</span>
                <span>中央コントローラー</span>
              </div>
              <dl className="inspector-list">
                <div><dt>プロセス</dt><dd>{coreProcess?.pid ? `PID ${coreProcess.pid}` : "未起動"}</dd></div>
                <div><dt>起動時刻</dt><dd>{formatDate(coreProcess?.started_at)}</dd></div>
                <div><dt>登録アセット</dt><dd>{assets.length} 件</dd></div>
                <div><dt>接続中</dt><dd>{connectedCount} / {snapshot.connections.length} 件</dd></div>
              </dl>
              <p className="inspector-empty">アセットまたは接続線を選択すると、実行情報と通信状況をここに固定表示します。</p>
            </>
          )}

          {selectedFlow && (
            <div className="selected-connection-card">
              <div>
                <span className="inspector-label">共有する PDU チャネル</span>
                <strong>{nameOf(selectedFlow.a)} ・ {nameOf(selectedFlow.b)}</strong>
              </div>
              <ul className="pdu-flow-channels">
                {selectedFlow.channels.map((channel) => <li key={`${channel.robot}:${channel.channel_id}`}><code>{channel.robot} ch{channel.channel_id}</code> {channel.org_name} <span className="muted">{nameOf(selectedFlow.a)} {directionArrow[channel.direction]} {nameOf(selectedFlow.b)}</span></li>)}
              </ul>
              {selectedFlow.channels.some((channel) => channel.direction === "shared") && <p className="muted">― は、どちらかが向きを宣言していない（チャネルを作るだけ、compact 形式など）ため、向きが分からないチャネルです。</p>}
            </div>
          )}

          {selectedConnection && (
            <div className="selected-connection-card">
              <div>
                <span className="inspector-label">選択中の通信</span>
                <strong>{selectedConnection.definition.label || "無名の接続"}</strong>
              </div>
              <span className={`status-badge status-${selectedConnection.state === "connected" ? "connected" : "disconnected"}`}>{selectedConnection.state === "connected" ? "接続中" : "切断"}</span>
              <p>{selectedConnection.definition.source} → {selectedConnection.definition.destination}</p>
              <button type="button" className="button subtle small" onClick={() => onHeartbeat(selectedConnection.definition.id)}>ハートビートを送信</button>
            </div>
          )}
        </aside>
      </div>

      <footer className="topology-footnote">
        <span><i className="legend-swatch legend-swatch-core" />Hakoniwa-core</span>
        <span><i className="legend-swatch legend-swatch-active" />起動・接続中</span>
        <span><i className="legend-swatch legend-swatch-idle" />停止・未接続</span>
        <span className="topology-motion-note">流れる線は接続済みの通信経路を示します。</span>
        <span><i className="legend-swatch legend-swatch-pdu" />PDU のチャネルの共有（矢印は宣言された書き手から読み手へ、破線は向き不明。数字はチャネル数）</span>
        <label className="toggle"><input type="checkbox" checked={showFlows} onChange={(event) => setShowFlows(event.target.checked)} />PDU の流れを表示</label>
        {!check && <span className="muted">接続チェックの結果を待っています。</span>}
      </footer>
    </section>
  );
}
