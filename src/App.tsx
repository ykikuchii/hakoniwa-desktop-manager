import { useEffect, useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api, newAsset } from "./api";
import type { AssetDefinition, ConnectionSnapshot, CoreCatalog, ImportPreview, Workspace, WorkspaceSnapshot } from "./types";
import { TopologyPage } from "./TopologyPage";
import { selectAssetProcess, selectCoreProcess } from "./selectors";
import "./topology-visualization.css";
import "./topology-alerts.css";

const tabs = [
  ["dashboard", "概要"],
  ["assets", "アセット"],
  ["topology", "接続・通信"],
  ["core", "Core管理"],
  ["workspace", "取込・設定"],
] as const;

type Tab = (typeof tabs)[number][0];

const fmt = new Intl.DateTimeFormat("ja-JP", { hour: "2-digit", minute: "2-digit", second: "2-digit" });

export default function App() {
  const [tab, setTab] = useState<Tab>("dashboard");
  const [snapshot, setSnapshot] = useState<WorkspaceSnapshot | null>(null);
  const [catalog, setCatalog] = useState<CoreCatalog | null>(null);
  const [notice, setNotice] = useState<string>("");
  const [error, setError] = useState<string>("");
  const [busy, setBusy] = useState(false);
  const [selectedAssetId, setSelectedAssetId] = useState<string | null>(null);
  const [draftAsset, setDraftAsset] = useState<AssetDefinition | null>(null);
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [selectedConnectionId, setSelectedConnectionId] = useState<string | null>(null);

  const refresh = async () => {
    try {
      const [next, coreCatalog] = await Promise.all([api.snapshot(), api.catalog()]);
      setSnapshot(next);
      setCatalog(coreCatalog);
    } catch (reason) {
      setError(messageOf(reason));
    }
  };

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 2500);
    return () => window.clearInterval(timer);
  }, []);

  const workspace = snapshot?.workspace;
  const storedAsset = workspace?.assets.find((asset) => asset.id === selectedAssetId) ?? null;
  const selectedAsset = storedAsset ?? (draftAsset && draftAsset.id === selectedAssetId ? draftAsset : null);
  const selectedConnection = snapshot?.connections.find((connection) => connection.definition.id === selectedConnectionId) ?? null;
  const activeCore = snapshot ? selectCoreProcess(snapshot.processes) : undefined;

  async function run(action: () => Promise<unknown>, message: string) {
    setBusy(true);
    setError("");
    try {
      await action();
      setNotice(message);
      await refresh();
    } catch (reason) {
      setError(messageOf(reason));
    } finally {
      setBusy(false);
    }
  }

  async function chooseDirectory() {
    const selection = await open({ directory: true, multiple: false, title: "Business PackまたはRecipeの設定ディレクトリを選択" });
    if (typeof selection !== "string") return;
    await run(async () => setPreview(await api.inspectDirectory(selection)), "構成を解析しました。内容を確認して適用してください。");
  }

  async function upsertAsset(asset: AssetDefinition) {
    await run(async () => {
      if (workspace?.assets.some((candidate) => candidate.id === asset.id)) await api.updateAsset(asset);
      else await api.createAsset(asset);
      setSelectedAssetId(asset.id);
      setDraftAsset((current) => (current?.id === asset.id ? null : current));
    }, "アセットを保存しました。");
  }

  function selectAsset(id: string) {
    setSelectedAssetId(id);
    setDraftAsset((current) => (current?.id === id ? current : null));
  }

  function createDraftAsset() {
    const asset = newAsset();
    setDraftAsset(asset);
    setSelectedAssetId(asset.id);
  }

  function removeAsset(id: string) {
    if (draftAsset?.id === id) {
      setDraftAsset(null);
      setSelectedAssetId(null);
      setNotice("未保存のアセットを破棄しました。");
      return;
    }
    void run(() => api.deleteAsset(id), "アセットを削除しました。");
  }

  if (!snapshot || !workspace) {
    return <main className="loading"><div className="pulse" /><p>Hakoniwa環境を確認しています。</p>{error && <p className="error">{error}</p>}</main>;
  }

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand"><div className="brand-mark">H</div><div><strong>Hakoniwa</strong><span>Desktop Manager</span></div></div>
        <nav>{tabs.map(([id, label]) => <button key={id} className={tab === id ? "nav-item active" : "nav-item"} onClick={() => setTab(id)}>{label}</button>)}</nav>
        <section className="environment"><span>実行環境</span><strong>{snapshot.platform} / {typeof snapshot.architecture === "string" ? snapshot.architecture : "other"}</strong><small>ローカル状態を2.5秒ごとに更新</small></section>
      </aside>
      <main className="main-content">
        <header className="topbar">
          <div><p className="eyebrow">WORKSPACE</p><h1>{workspace.name}</h1></div>
          <div className="top-actions">
            <button className="button secondary" disabled={busy} onClick={() => void run(() => api.stopAll(), "すべての管理対象プロセスを停止しました。")}>すべて停止</button>
            <button className="button primary" disabled={busy} onClick={() => void run(() => api.startAll(), "Coreと有効なアセットの起動を開始しました。")}>一括起動</button>
          </div>
        </header>
        {(notice || error) && <div className={error ? "banner error" : "banner"}><span>{error || notice}</span><button onClick={() => { setError(""); setNotice(""); }}>閉じる</button></div>}
        {tab === "dashboard" && <Dashboard snapshot={snapshot} activeCore={activeCore?.status ?? "unknown"} onStartCore={() => void run(api.startCore, "Coreコントローラーを起動しました。")} onStopCore={() => void run(api.stopCore, "Coreコントローラーを停止しました。")} onLifecycle={(command) => void run(async () => { const result = await api.runLifecycle(command); if (result.status === "failed") throw new Error(result.stderr || "hako-cmdが失敗しました。"); }, `hako-cmd ${command} を実行しました。`)} onAssetClick={(id) => { setSelectedAssetId(id); setTab("assets"); }} onConnectionClick={(id) => { setSelectedConnectionId(id); setTab("topology"); }} />}
        {tab === "assets" && <AssetsPage workspace={workspace} processes={snapshot.processes} selected={selectedAsset} draft={draftAsset} busy={busy} onSelect={selectAsset} onSave={upsertAsset} onDelete={removeAsset} onStart={(id) => void run(() => api.startAsset(id), "アセットを起動しました。")} onStop={(id) => void run(() => api.stopAsset(id), "アセットを停止しました。")} onCreate={createDraftAsset} />}
        {tab === "topology" && <TopologyPage snapshot={snapshot} selected={selectedConnection} onSelect={setSelectedConnectionId} onHeartbeat={(id) => void run(() => api.recordHeartbeat(id, "ユーザー操作による確認イベント"), "通信確認イベントを記録しました。")} />}
        {tab === "core" && <CorePage workspace={workspace} catalog={catalog} activeCore={activeCore?.status ?? "unknown"} busy={busy} onSaveWorkspace={(next) => void run(() => api.saveWorkspace(next), "Core設定を保存しました。")} onInstall={(version) => void run(() => api.installCore(version), "Coreを検証して導入しました。")} onStart={() => void run(api.startCore, "Coreコントローラーを起動しました。")} onStop={() => void run(api.stopCore, "Coreコントローラーを停止しました。")} />}
        {tab === "workspace" && <WorkspacePage workspace={workspace} preview={preview} onChoose={() => void chooseDirectory()} onApply={() => preview && void run(() => api.applyPreview(preview), "インポート結果をワークスペースへ適用しました。")} onSave={(next) => void run(() => api.saveWorkspace(next), "ワークスペースを保存しました。")} />}
      </main>
    </div>
  );
}

function Dashboard({ snapshot, activeCore, onStartCore, onStopCore, onLifecycle, onAssetClick, onConnectionClick }: { snapshot: WorkspaceSnapshot; activeCore: string; onStartCore: () => void; onStopCore: () => void; onLifecycle: (command: "start" | "stop" | "reset") => void; onAssetClick: (id: string) => void; onConnectionClick: (id: string) => void }) {
  const running = snapshot.processes.filter((process) => ["running", "starting"].includes(process.status)).length;
  const connected = snapshot.connections.filter((connection) => connection.state === "connected").length;
  const latest = snapshot.recent_events.slice(0, 6);
  return <section className="page-grid">
    <div className="status-hero"><div><p className="eyebrow">SIMULATION CONTROL</p><h2>Coreとアセットの状態を一目で把握</h2><p>OSプロセス、シミュレーション時刻、PDU／ネットワーク経路の観測を分けて表示します。</p></div><div className={`core-orb ${activeCore}`}><span>{activeCore === "running" ? "稼働中" : activeCore === "starting" ? "起動中" : "停止中"}</span><strong>Core</strong></div></div>
    <div className="metric-row"><Metric label="管理プロセス" value={`${running} / ${snapshot.processes.length}`} hint="起動中 / 履歴" /><Metric label="通信中の経路" value={`${connected} / ${snapshot.connections.length}`} hint="直近15秒に観測" /><Metric label="登録アセット" value={snapshot.workspace.assets.length} hint="個別に起動・停止可能" /><Metric label="直近イベント" value={snapshot.recent_events.length} hint="最大250件を表示" /></div>
    <div className="panel core-panel"><div className="panel-heading"><div><p className="eyebrow">CORE LIFECYCLE</p><h3>シミュレーション制御</h3></div><StatusBadge value={activeCore} /></div><p className="muted">CoreコントローラーのOSプロセスと、<code>hako-cmd</code>の時刻制御は別の操作です。</p><div className="button-row"><button className="button primary" onClick={onStartCore}>Coreを起動</button><button className="button secondary" onClick={onStopCore}>Coreを停止</button><button className="button subtle" onClick={() => onLifecycle("start")}>時刻を開始</button><button className="button subtle" onClick={() => onLifecycle("stop")}>時刻を停止</button><button className="button subtle" onClick={() => onLifecycle("reset")}>リセット</button></div></div>
    <div className="panel"><div className="panel-heading"><div><p className="eyebrow">ASSETS</p><h3>アセットの実行状況</h3></div><span className="muted">クリックして編集</span></div><div className="list">{snapshot.workspace.assets.length === 0 ? <Empty label="まだアセットがありません。設定を取り込むか、アセット画面から追加してください。" /> : snapshot.workspace.assets.map((asset) => { const process = selectAssetProcess(snapshot.processes, asset.id); return <button className="list-item interactive" key={asset.id} onClick={() => onAssetClick(asset.id)}><span className="dot" data-state={process?.status ?? "unknown"} /><span className="item-main"><strong>{asset.name}</strong><small>{asset.role} · {asset.activation_timing}</small></span><StatusBadge value={process?.status ?? "unknown"} /></button>; })}</div></div>
    <div className="panel"><div className="panel-heading"><div><p className="eyebrow">COMMUNICATION</p><h3>接続経路</h3></div><span className="muted">選択して詳細を表示</span></div><div className="list">{snapshot.connections.length === 0 ? <Empty label="EndpointまたはBridge設定を取り込むと、接続経路を表示します。" /> : snapshot.connections.map((connection) => <button className="list-item interactive" key={connection.definition.id} onClick={() => onConnectionClick(connection.definition.id)}><span className="dot" data-state={connection.state} /><span className="item-main"><strong>{connection.definition.source} <i>→</i> {connection.definition.destination}</strong><small>{connection.definition.transport} · 最終通信 {connection.last_activity_at ? fmt.format(new Date(connection.last_activity_at)) : "未観測"}</small></span><StatusBadge value={connection.state} /></button>)}</div></div>
    <div className="panel timeline"><div className="panel-heading"><div><p className="eyebrow">OBSERVATIONS</p><h3>直近の通信イベント</h3></div></div>{latest.length === 0 ? <Empty label="まだ通信イベントはありません。Bridge monitorやEndpointログを接続するとここに表示されます。" /> : latest.map((event) => <div className="event" key={event.id}><span className="event-time">{fmt.format(new Date(event.observed_at))}</span><span className="event-type">{event.event_type}</span><span>{event.message || event.pdu_name || "イベント"}</span></div>)}</div>
  </section>;
}

function AssetsPage({ workspace, processes, selected, draft, busy, onSelect, onSave, onDelete, onStart, onStop, onCreate }: { workspace: Workspace; processes: WorkspaceSnapshot["processes"]; selected: AssetDefinition | null; draft: AssetDefinition | null; busy: boolean; onSelect: (id: string) => void; onSave: (asset: AssetDefinition) => void; onDelete: (id: string) => void; onStart: (id: string) => void; onStop: (id: string) => void; onCreate: () => void }) {
  const unsaved = draft && !workspace.assets.some((asset) => asset.id === draft.id) ? draft : null;
  return <section className="split-page"><div className="panel asset-list-panel"><div className="panel-heading"><div><p className="eyebrow">ASSET CATALOG</p><h2>個別アセット</h2></div><button className="button primary small" disabled={busy} onClick={onCreate}>追加</button></div><p className="muted">コマンド、引数、作業ディレクトリ、環境変数、起動順序を明示的に管理します。</p><div className="asset-cards">{workspace.assets.map((asset) => { const process = selectAssetProcess(processes, asset.id); return <button key={asset.id} className={selected?.id === asset.id ? "asset-card selected" : "asset-card"} onClick={() => onSelect(asset.id)}><span className="dot" data-state={process?.status ?? "unknown"} /><strong>{asset.name}</strong><small>{asset.role} · {asset.command.target === "native" ? "ローカル" : "WSL"}</small></button>; })}{unsaved && <button key={unsaved.id} className={selected?.id === unsaved.id ? "asset-card selected" : "asset-card"} onClick={() => onSelect(unsaved.id)}><span className="dot" data-state="unknown" /><strong>{unsaved.name}</strong><small>未保存 · 実行ファイルを入力して保存してください</small></button>}{workspace.assets.length === 0 && !unsaved && <Empty label="アセットがありません。" />}</div></div><div className="panel editor-panel">{selected ? <AssetEditor key={selected.id} asset={selected} allAssets={workspace.assets} busy={busy} persisted={selected.id !== unsaved?.id} onSave={onSave} onDelete={() => onDelete(selected.id)} onStart={() => onStart(selected.id)} onStop={() => onStop(selected.id)} /> : <Empty label="左側からアセットを選択してください。" />}</div></section>;
}

function AssetEditor({ asset, allAssets, busy, persisted, onSave, onDelete, onStart, onStop }: { asset: AssetDefinition; allAssets: AssetDefinition[]; busy: boolean; persisted: boolean; onSave: (asset: AssetDefinition) => void; onDelete: () => void; onStart: () => void; onStop: () => void }) {
  const [draft, setDraft] = useState(asset);
  const patch = <K extends keyof AssetDefinition>(key: K, value: AssetDefinition[K]) => setDraft((current) => ({ ...current, [key]: value }));
  const patchCommand = (key: keyof AssetDefinition["command"], value: unknown) => setDraft((current) => ({ ...current, command: { ...current.command, [key]: value } }));
  return <><div className="panel-heading"><div><p className="eyebrow">ASSET EDITOR</p><h2>{draft.name}</h2></div><div className="button-row"><button className="button subtle" disabled={busy || !persisted} onClick={onStart}>起動</button><button className="button subtle" disabled={busy || !persisted} onClick={onStop}>停止</button></div></div>{!persisted && <p className="muted">未保存のアセットです。実行ファイルを入力して保存すると、起動・停止できるようになります。</p>}<div className="form-grid"><Field label="表示名"><input value={draft.name} onChange={(event) => patch("name", event.target.value)} /></Field><Field label="役割"><select value={draft.role} onChange={(event) => patch("role", event.target.value as AssetDefinition["role"])}>{["simulator","controller","visualizer","bridge","service","external_client","monitor","other"].map((role) => <option key={role}>{role}</option>)}</select></Field><Field label="実行ファイル / コマンド" wide><input value={draft.command.program} placeholder="例: python3.12" onChange={(event) => patchCommand("program", event.target.value)} /><small>シェル文字列ではなく実行ファイルを指定し、引数は下欄で分離します。</small></Field><Field label="引数（1行に1個）" wide><textarea value={draft.command.args.join("\n")} onChange={(event) => patchCommand("args", event.target.value.split("\n").map((value) => value.trim()).filter(Boolean))} /></Field><Field label="作業ディレクトリ" wide><input value={draft.command.cwd ?? ""} onChange={(event) => patchCommand("cwd", event.target.value || null)} /></Field><Field label="実行環境"><select value={draft.command.target === "native" ? "native" : "wsl"} onChange={(event) => patchCommand("target", event.target.value === "native" ? "native" : { wsl: { distribution: "Ubuntu" } })}><option value="native">ローカルOS</option><option value="wsl">WSL2</option></select></Field>{draft.command.target !== "native" && <Field label="WSLディストリビューション"><input value={draft.command.target.wsl.distribution} onChange={(event) => patchCommand("target", { wsl: { distribution: event.target.value } })} /></Field>}<Field label="起動タイミング"><select value={draft.activation_timing} onChange={(event) => patch("activation_timing", event.target.value as AssetDefinition["activation_timing"])}><option value="before_start">before_start</option><option value="manual">manual</option><option value="after_start">after_start</option></select></Field><Field label="依存先"><select multiple value={draft.depends_on} onChange={(event) => patch("depends_on", Array.from(event.target.selectedOptions).map((option) => option.value))}><option value="core">core</option>{allAssets.filter((candidate) => candidate.id !== draft.id).map((candidate) => <option key={candidate.id} value={candidate.id}>{candidate.name}</option>)}</select><small>複数選択できます。</small></Field><Field label="環境変数（KEY=VALUE、1行に1個）" wide><textarea value={Object.entries(draft.command.env).map(([key, value]) => `${key}=${value}`).join("\n")} onChange={(event) => patchCommand("env", parseEnv(event.target.value))} /></Field><label className="toggle"><input type="checkbox" checked={draft.enabled} onChange={(event) => patch("enabled", event.target.checked)} />起動対象に含める</label></div><div className="form-actions"><button className="button danger" disabled={busy} onClick={onDelete}>{persisted ? "削除" : "破棄"}</button><button className="button primary" disabled={busy || draft.command.program.trim() === "" || draft.name.trim() === ""} onClick={() => onSave(draft)}>保存</button></div></>;
}

function CorePage({ workspace, catalog, activeCore, busy, onSaveWorkspace, onInstall, onStart, onStop }: { workspace: Workspace; catalog: CoreCatalog | null; activeCore: string; busy: boolean; onSaveWorkspace: (workspace: Workspace) => void; onInstall: (version: string) => void; onStart: () => void; onStop: () => void }) {
  const [controller, setController] = useState(workspace.core_controller ?? { id: crypto.randomUUID(), name: "Hakoniwa Core Controller", command: { program: "", args: [], cwd: null, env: {}, target: "native" as const }, readiness: { kind: "manual" as const } });
  const currentVersion = workspace.core_release?.version;
  return <section className="core-page"><div className="panel"><div className="panel-heading"><div><p className="eyebrow">APPROVED CORE RELEASES</p><h2>Coreの導入と切替</h2></div><StatusBadge value={activeCore} /></div>{catalog && catalog.releases.length > 0 ? <div className="release-list">{catalog.releases.map((release) => <div className="release" key={release.version}><div><strong>hakoniwa-core-pro {release.version}</strong><small>revision: {release.source_revision} · 承認済みアーティファクト {release.artifacts.length}件</small></div><div>{currentVersion === release.version ? <span className="installed">選択中</span> : <button className="button primary small" disabled={busy} onClick={() => onInstall(release.version)}>検証して導入</button>}</div></div>)}</div> : <div className="empty-state"><strong>承認済みのOS別バイナリが未登録です。</strong><p>公式リリースには現時点でOS別バイナリ配布の整合性情報が揃っていません。リリースCIで生成した署名・SHA-256付きのアーティファクトを、管理者が承認カタログへ追加してください。</p></div>}<p className="security-note">導入時はHTTPS取得、SHA-256照合、zip-slip防止、アトミック配置を行います。検証に失敗した実行ファイルは利用しません。</p></div><div className="panel"><div className="panel-heading"><div><p className="eyebrow">CORE CONTROLLER</p><h2>実行コントローラー</h2></div><div className="button-row"><button className="button subtle" onClick={onStart}>起動</button><button className="button subtle" onClick={onStop}>停止</button></div></div><p className="muted">例: Conductor所有アセットやLauncherを指定します。<code>hako-cmd start</code>は別途ライフサイクル制御として実行されます。</p><div className="form-grid"><Field label="名称"><input value={controller.name} onChange={(event) => setController({ ...controller, name: event.target.value })} /></Field><Field label="実行ファイル / コマンド" wide><input value={controller.command.program} placeholder="例: /path/to/hako-conductor または python3.12" onChange={(event) => setController({ ...controller, command: { ...controller.command, program: event.target.value } })} /></Field><Field label="引数（1行に1個）" wide><textarea value={controller.command.args.join("\n")} onChange={(event) => setController({ ...controller, command: { ...controller.command, args: event.target.value.split("\n").map((value) => value.trim()).filter(Boolean) } })} /></Field><Field label="作業ディレクトリ" wide><input value={controller.command.cwd ?? ""} onChange={(event) => setController({ ...controller, command: { ...controller.command, cwd: event.target.value || null } })} /></Field></div><div className="form-actions"><button className="button primary" disabled={busy} onClick={() => onSaveWorkspace({ ...workspace, core_controller: controller })}>コントローラー設定を保存</button></div></div></section>;
}

function WorkspacePage({ workspace, preview, onChoose, onApply, onSave }: { workspace: Workspace; preview: ImportPreview | null; onChoose: () => void; onApply: () => void; onSave: (workspace: Workspace) => void }) {
  const [name, setName] = useState(workspace.name);
  return <section className="workspace-page"><div className="panel"><div className="panel-heading"><div><p className="eyebrow">BUSINESS PACK IMPORT</p><h2>設定ディレクトリを取り込む</h2></div><button className="button primary" onClick={onChoose}>ディレクトリを選択</button></div><p className="muted">Launcherのassets、Endpointのtransport、Bridgeの経路を読み取り、ワークスペースへ反映します。元ファイルは変更しません。</p>{preview && <div className="import-preview"><div className="metric-row"><Metric label="検出ファイル" value={preview.discovered_files.length} hint="JSON設定" /><Metric label="アセット" value={preview.assets.length} hint="Launcher assets[]" /><Metric label="接続" value={preview.connections.length} hint="Endpoint / Bridge" /><Metric label="注意" value={preview.warnings.length} hint="確認が必要" /></div><div className="warning-list">{preview.warnings.map((warning) => <p key={warning}>{warning}</p>)}</div><div className="form-actions"><button className="button primary" onClick={onApply}>この結果を適用</button></div></div>}</div><div className="panel"><div className="panel-heading"><div><p className="eyebrow">WORKSPACE</p><h2>ローカル保存設定</h2></div></div><div className="form-grid"><Field label="ワークスペース名" wide><input value={name} onChange={(event) => setName(event.target.value)} /></Field><Field label="取込元ディレクトリ" wide><input value={workspace.source_directory ?? "未設定"} disabled /></Field></div><div className="form-actions"><button className="button primary" onClick={() => onSave({ ...workspace, name })}>保存</button></div></div></section>;
}

function Metric({ label, value, hint }: { label: string; value: string | number; hint: string }) { return <div className="metric"><span>{label}</span><strong>{value}</strong><small>{hint}</small></div>; }
function StatusBadge({ value }: { value: string }) { return <span className="status-badge" data-state={value}>{labelFor(value)}</span>; }
function Field({ label, wide, children }: { label: string; wide?: boolean; children: React.ReactNode }) { return <label className={wide ? "field wide" : "field"}><span>{label}</span>{children}</label>; }
function Empty({ label }: { label: string }) { return <div className="empty-state">{label}</div>; }
function EventTable({ events }: { events: WorkspaceSnapshot["recent_events"] }) { return <div className="events-table"><h3>通信タイムライン</h3>{events.length === 0 ? <Empty label="この経路のイベントはまだありません。" /> : events.slice(0, 50).map((event) => <div className="event" key={event.id}><span className="event-time">{fmt.format(new Date(event.observed_at))}</span><span className="event-type">{event.event_type}</span><span>{event.direction}</span><code>{event.pdu_name ?? "—"}</code><span>{event.byte_count ?? "—"}</span><span>{event.message}</span></div>)}</div>; }
function parseEnv(value: string) { return Object.fromEntries(value.split("\n").map((line) => line.trim()).filter(Boolean).map((line) => { const separator = line.indexOf("="); return separator < 0 ? [line, ""] : [line.slice(0, separator).trim(), line.slice(separator + 1)]; })); }
function messageOf(reason: unknown) { return reason instanceof Error ? reason.message : String(reason); }
function labelFor(value: string) { return ({ running: "稼働中", starting: "起動中", stopping: "停止中", exited: "終了", failed: "失敗", connected: "通信中", idle: "待機", disconnected: "切断", unknown: "未確認" } as Record<string, string>)[value] ?? value; }
