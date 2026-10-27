import { useEffect, useMemo, useRef, useState } from "react";
import type { ConnectionSnapshot, PduFlow, ProcessSnapshot, ProjectCheck, WorkspaceSnapshot } from "./types";
import { TopologyAlertOverlay } from "./TopologyAlertOverlay";
import { connectionsForAsset, latestActivity, selectAssetProcess, selectCoreProcess, selectCoreStatus } from "./selectors";

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

/** 二次ベジェ（p0, c, p2）の t の点。 */
function bezierAt(p0: Position, c: Position, p2: Position, t: number): Position {
  const u = 1 - t;
  return { x: u * u * p0.x + 2 * u * t * c.x + t * t * p2.x, y: u * u * p0.y + 2 * u * t * c.y + t * t * p2.y };
}

const FLOW_SAMPLES = 64;

/**
 * 2 点（画素）を結ぶ曲線。中央の Core と重ならないよう、曲線のどの点も中心から `clearance` 以上
 * 離れるまで、中心から離れる側へ膨らませる。両端はカードの縁で切り、ラベルは切った曲線の中ほどに置く。
 */
// node はカード（114×74 px）の半分の大きさに、矢印の分の余白を足したもの。
function flowPath(from: Position, to: Position, center: Position, clearance: number, bounds: { width: number; height: number }, node = { halfWidth: 61, halfHeight: 41 }) {
  const mx = (from.x + to.x) / 2;
  const my = (from.y + to.y) / 2;
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const length = Math.hypot(dx, dy) || 1;
  let nx = -dy / length;
  let ny = dx / length;
  // 中心から離れる側へ曲げる。
  if ((mx - center.x) * nx + (my - center.y) * ny < 0) { nx = -nx; ny = -ny; }
  const closest = (c: Position) => {
    let min = Infinity;
    for (let i = 0; i <= FLOW_SAMPLES; i++) {
      const p = bezierAt(from, c, to, i / FLOW_SAMPLES);
      min = Math.min(min, Math.hypot(p.x - center.x, p.y - center.y));
    }
    return min;
  };
  // 制御点は枠の中に収める（曲線は両端と制御点の作る三角形の中にあるので、枠からはみ出さない）。
  const controlFor = (bend: number) => ({
    x: Math.min(Math.max(mx + nx * bend, 0), bounds.width),
    y: Math.min(Math.max(my + ny * bend, 0), bounds.height),
  });
  let bend = length * 0.12;
  let control = controlFor(bend);
  for (let i = 0; i < 24 && closest(control) < clearance; i++) {
    bend = bend * 1.25 + 4;
    const next = controlFor(bend);
    // 枠の端に当たってそれ以上曲げられなければ、そこで止める。
    if (next.x === control.x && next.y === control.y) break;
    control = next;
  }
  // 両端はノードの中心ではなく、カード（長方形）の縁で止める（矢印がカードに隠れないように）。
  // 曲線がカードの外へ出る t を求め、その区間を切り出す。外へ出なければ（カードが重なるほど近い）切らない。
  const inside = (p: Position, nodeCenter: Position) => Math.abs(p.x - nodeCenter.x) <= node.halfWidth && Math.abs(p.y - nodeCenter.y) <= node.halfHeight;
  // 内側の t と外側の t の間を二分探索して、縁の位置を詰める。
  const boundary = (tIn: number, tOut: number, nodeCenter: Position) => {
    for (let i = 0; i < 20; i++) {
      const t = (tIn + tOut) / 2;
      if (inside(bezierAt(from, control, to, t), nodeCenter)) tIn = t; else tOut = t;
    }
    return tOut;
  };
  let t0 = 0;
  let t1 = 1;
  for (let i = 0; i <= FLOW_SAMPLES; i++) {
    const t = i / FLOW_SAMPLES;
    if (!inside(bezierAt(from, control, to, t), from)) { t0 = i === 0 ? 0 : boundary((i - 1) / FLOW_SAMPLES, t, from); break; }
  }
  for (let i = FLOW_SAMPLES; i >= 0; i--) {
    const t = i / FLOW_SAMPLES;
    if (!inside(bezierAt(from, control, to, t), to)) { t1 = i === FLOW_SAMPLES ? 1 : boundary((i + 1) / FLOW_SAMPLES, t, to); break; }
  }
  if (t1 - t0 < 1 / FLOW_SAMPLES) { t0 = 0; t1 = 1; }
  // [t0, t1] の区間も二次ベジェで、制御点は blossom (t0, t1)。
  const start = bezierAt(from, control, to, t0);
  const end = bezierAt(from, control, to, t1);
  const w0 = (1 - t0) * (1 - t1);
  const w1 = (1 - t0) * t1 + t0 * (1 - t1);
  const w2 = t0 * t1;
  const c = { x: w0 * from.x + w1 * control.x + w2 * to.x, y: w0 * from.y + w1 * control.y + w2 * to.y };
  return { d: `M ${start.x} ${start.y} Q ${c.x} ${c.y} ${end.x} ${end.y}`, curve: { start, c, end }, label: bezierAt(start, c, end, 0.5) };
}

function distanceToSegment(p: Position, a: Position, b: Position) {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const t = dx || dy ? Math.min(Math.max(((p.x - a.x) * dx + (p.y - a.y) * dy) / (dx * dx + dy * dy), 0), 1) : 0;
  return Math.hypot(p.x - (a.x + dx * t), p.y - (a.y + dy * t));
}

/** 点から、中心 `center`・半分の大きさ（halfWidth, halfHeight）の長方形までの距離（内側なら 0）。 */
function distanceToRect(p: Position, center: Position, halfWidth: number, halfHeight: number) {
  return Math.hypot(Math.max(Math.abs(p.x - center.x) - halfWidth, 0), Math.max(Math.abs(p.y - center.y) - halfHeight, 0));
}

// ラベル（直径 20 px）を、ほかの線やラベルから離れたとみなす距離。
const LABEL_CLEARANCE = 22;
const LABEL_CANDIDATES = 24;
const CURVE_SAMPLES = 32;

type Curve = { start: Position; c: Position; end: Position };

/**
 * 各曲線のラベルを、ほかのものから離れた位置に置く。避けるのは、Core から各ノードへの線、ノードのカード、
 * ほかの流れの曲線、先に置いたラベル。曲線の中ほど（t = 0.2〜0.8）から、近くのものとの距離が十分な点のうち、
 * 前回置いた t（なければ中央）に近い点を選ぶ。キーの順に置くので、流れの並び順によらない。
 */
function placeLabels(items: { key: string; curve: Curve }[], spokes: [Position, Position][], cards: Position[], previous: Map<string, number>) {
  const samples = new Map(items.map(({ key, curve }) => [key, Array.from({ length: CURVE_SAMPLES + 1 }, (_, i) => bezierAt(curve.start, curve.c, curve.end, i / CURVE_SAMPLES))]));
  const placed: Position[] = [];
  const result = new Map<string, { point: Position; t: number }>();
  for (const { key, curve } of [...items].sort((x, y) => (x.key < y.key ? -1 : x.key > y.key ? 1 : 0))) {
    const anchor = previous.get(key) ?? 0.5;
    let best = { point: bezierAt(curve.start, curve.c, curve.end, anchor), t: anchor };
    let bestScore = -Infinity;
    for (let i = 0; i <= LABEL_CANDIDATES; i++) {
      const t = 0.2 + (i / LABEL_CANDIDATES) * 0.6;
      const p = bezierAt(curve.start, curve.c, curve.end, t);
      let near = Infinity;
      for (const [a, b] of spokes) near = Math.min(near, distanceToSegment(p, a, b));
      // カードは 114×74 px。
      for (const card of cards) near = Math.min(near, distanceToRect(p, card, 57, 37));
      for (const [other, points] of samples) {
        if (other === key) continue;
        for (let j = 1; j < points.length; j++) near = Math.min(near, distanceToSegment(p, points[j - 1], points[j]));
      }
      for (const q of placed) near = Math.min(near, Math.hypot(p.x - q.x, p.y - q.y));
      // 十分離れていれば、それ以上は区別せず前回の位置（なければ中央）に近い点を選ぶ。
      const score = Math.min(near, LABEL_CLEARANCE) - Math.abs(t - anchor) * 4;
      if (score > bestScore) { bestScore = score; best = { point: p, t }; }
    }
    placed.push(best.point);
    result.set(key, best);
  }
  return result;
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
  const coreState = selectCoreStatus(snapshot);
  // Core コントローラーのないプロジェクトでは、Core のプロセスは表示できない。開始したかだけ分かる。
  const noController = !snapshot.workspace.core_controller;
  const coreProcessLabel = noController ? "コントローラーなし" : coreProcess?.pid ? `PID ${coreProcess.pid}` : "未起動";
  const coreStartLabel = noController ? (snapshot.core_started ? "hako-cmd start で開始済み" : "hako-cmd start 前") : formatDate(coreProcess?.started_at);
  // 共有メモリの PDU は通信量を観測していない。接続チェックで分かる、宣言上のチャネルだけを出す。
  const pduChannels = (memberId: string) => check?.members.find((member) => member.member_id === memberId)?.channels ?? [];
  const coreStatus: VisualStatus = coreState === "failed"
    ? "error"
    : coreState === "starting"
      ? "starting"
      : coreState === "running"
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
  const labelTs = useRef(new Map<string, number>());
  // 線は表示していて、枠の大きさが分かっているときだけ求める。
  const drawn = useMemo(() => {
    if (!showFlows || stageSize.width <= 0 || stageSize.height <= 0) return [];
    const toPixels = (position: Position) => ({ x: (position.x / 100) * stageSize.width, y: (position.y / 100) * stageSize.height });
    const center = toPixels({ x: 50, y: 50 });
    // Core（直径 142 px）の半径に余白を足したものを下限にする。
    const clearance = Math.max(Math.min(stageSize.width, stageSize.height) * 0.16, 83);
    const paths = flows.map((flow) => flowPath(toPixels(positions.get(flow.a)!), toPixels(positions.get(flow.b)!), center, clearance, stageSize));
    // Core から各ノードへの線（接続の線）にラベルが乗らないようにする。
    const spokes = [...positions.values()].map((position): [Position, Position] => [center, toPixels(position)]);
    const cards = [...positions.values()].map(toPixels);
    const labels = placeLabels(flows.map((flow, index) => ({ key: flowKey(flow), curve: paths[index].curve })), spokes, cards, labelTs.current);
    return flows.map((flow, index) => {
      const directions = new Set(flow.channels.map((channel) => channel.direction));
      return {
        flow,
        key: flowKey(flow),
        path: { ...paths[index], label: labels.get(flowKey(flow))!.point },
        labelT: labels.get(flowKey(flow))!.t,
        toB: directions.has("a_to_b") || directions.has("both"),
        toA: directions.has("b_to_a") || directions.has("both"),
      };
    });
  }, [flows, positions, showFlows, stageSize]);
  // 次に置き直すとき（枠の大きさが変わったときなど）にラベルが跳ばないよう、置いた位置を覚えておく。
  useEffect(() => {
    if (drawn.length > 0) labelTs.current = new Map(drawn.map((item) => [item.key, item.labelT]));
  }, [drawn]);
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
              <span>{statusLabels[coreStatus]} · {coreProcessLabel}</span>
              <span>起動: {coreStartLabel}</span>
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
              {hoveredNode.connections.length > 0
                ? <span>接続 {hoveredNode.connections.filter((connection) => connection.state === "connected").length} 件 · 最終通信 {formatDate(latestActivity(hoveredNode.connections))}</span>
                : <span>PDU チャネル {pduChannels(hoveredNode.id).length} 本 · 通信量は未観測</span>}
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
                <div><dt>PDU チャネル</dt><dd>{pduChannels(detailNode.id).length} 本（登録 {pduChannels(detailNode.id).filter((channel) => channel.registers).length} 本）</dd></div>
                {detailNode.connections.length > 0 ? (
                  <>
                    <div><dt>接続数</dt><dd>{detailNode.connections.length} 件（接続中 {detailNode.connections.filter((connection) => connection.state === "connected").length} 件）</dd></div>
                    <div><dt>最終通信</dt><dd>{formatDate(latestActivity(detailNode.connections))}</dd></div>
                  </>
                ) : <div><dt>通信量</dt><dd>未観測（共有メモリの PDU は観測していません）</dd></div>}
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
                <div><dt>プロセス</dt><dd>{coreProcessLabel}</dd></div>
                <div><dt>起動時刻</dt><dd>{coreStartLabel}</dd></div>
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
