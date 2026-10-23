import { useEffect, useMemo, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "./api";
import type { CoreCatalog, ImportPreview, ProjectCheck, Workspace, WorkspaceSnapshot } from "./types";
import { CheckPanel } from "./CheckPanel";
import { ChecksPage } from "./ChecksPage";
import { LibraryPage } from "./LibraryPage";
import { ProjectFilePanel } from "./ProjectFilePanel";
import { ProjectPage } from "./ProjectPage";
import { Empty, Field, messageOf } from "./ui";
import { TopologyPage } from "./TopologyPage";
import { selectAssetProcess, selectCoreProcess } from "./selectors";
import "./topology-visualization.css";
import "./topology-alerts.css";

const tabs = [
  ["dashboard", "概要"],
  ["project", "プロジェクト"],
  ["library", "ライブラリ"],
  ["checks", "接続チェック"],
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
  const [libraryFocus, setLibraryFocus] = useState<string | null>(null);
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [selectedConnectionId, setSelectedConnectionId] = useState<string | null>(null);
  const [check, setCheck] = useState<ProjectCheck | null>(null);
  // 接続チェックはファイルを読むので、概要か接続チェックを開いているときだけ確かめる。
  const tabRef = useRef<Tab>("dashboard");
  tabRef.current = tab;
  // 接続チェックの問い合わせの世代。切り替えのたびに進め、古い問い合わせの結果
  // （成功も失敗も）は捨てる。同じプロジェクトでも、後から出した問い合わせを優先する。
  const checkGeneration = useRef(0);
  const [checkError, setCheckError] = useState<string>("");

  const refresh = async () => {
    try {
      const [next, coreCatalog] = await Promise.all([api.snapshot(), api.catalog()]);
      setSnapshot(next);
      setCatalog(coreCatalog);
      // 接続チェックが失敗しても、ほかの表示は続ける。結果は、表示中のプロジェクトのもの
      // だけを使う（切り替えの前に出した問い合わせの結果は捨てる）。
      if (tabRef.current === "dashboard" || tabRef.current === "checks" || tabRef.current === "topology") {
        const shownProject = next.running_project_id ?? next.active_project_id;
        const generation = ++checkGeneration.current;
        api.checkProject().then(
          (result) => { if (generation === checkGeneration.current && result.project_id === shownProject) { setCheck(result); setCheckError(""); } },
          (reason) => { if (generation === checkGeneration.current) { setCheck(null); setCheckError(messageOf(reason)); } },
        );
      }
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

  async function importV1() {
    const selection = await open({ directory: false, multiple: false, title: "v1 の workspace.json を選択", filters: [{ name: "JSON", extensions: ["json"] }] });
    if (typeof selection !== "string") return;
    const replace = window.confirm("同じ出所から取り込んだプロジェクトがあれば、置き換えますか？\n［キャンセル］を選ぶと、新しいプロジェクトとして取り込みます。");
    setSelectedAssetId(null);
    setLibraryFocus(null);
    setSelectedConnectionId(null);
    setPreview(null);
    setCheck(null);
    checkGeneration.current++;
    await run(() => api.importV1Workspace(selection, replace), "v1 のワークスペースを取り込みました。");
  }

  /** 表示中のプロジェクトが変わるときに、選択と接続チェックの結果を捨てる。 */
  function resetView() {
    setSelectedAssetId(null);
    setLibraryFocus(null);
    setSelectedConnectionId(null);
    setPreview(null);
    setCheck(null);
    checkGeneration.current++;
  }

  function switchProject(projectId: string) {
    setSelectedAssetId(null);
    setLibraryFocus(null);
    setSelectedConnectionId(null);
    setPreview(null);
    setCheck(null);
    checkGeneration.current++;
    void run(() => api.selectProject(projectId), "プロジェクトを切り替えました。");
  }

  function addProject() {
    const name = window.prompt("新しいプロジェクトの名前");
    if (!name?.trim()) return;
    setSelectedAssetId(null);
    setLibraryFocus(null);
    setSelectedConnectionId(null);
    setPreview(null);
    setCheck(null);
    checkGeneration.current++;
    void run(() => api.createProject(name), "プロジェクトを作りました。");
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
          <div>
            <p className="eyebrow">PROJECT</p>
            <div className="project-switcher">
              <select aria-label="プロジェクト" value={snapshot.active_project_id} disabled={busy || !!snapshot.running_project_id} title={snapshot.running_project_id ? "実行中は切り替えられません。すべて停止してください。" : undefined} onChange={(event) => switchProject(event.target.value)}>
                {snapshot.projects.map((project) => <option key={project.id} value={project.id}>{project.name}</option>)}
              </select>
              <button className="button subtle small" disabled={busy || !!snapshot.running_project_id} onClick={addProject}>新規</button>
              {snapshot.running_project_id && <span className="muted">実行中</span>}
            </div>
          </div>
          <div className="top-actions">
            <button className="button secondary" disabled={busy} onClick={() => void run(() => api.stopAll(), "すべての管理対象プロセスを停止しました。")}>すべて停止</button>
            <button className="button primary" disabled={busy} onClick={() => void run(() => api.startAll(), "Coreと有効なアセットの起動を開始しました。")}>一括起動</button>
          </div>
        </header>
        {snapshot.store_warnings.length > 0 && <div className="banner error"><div>{snapshot.store_warnings.map((warning, index) => <p key={index}>{warning}</p>)}</div><button onClick={() => void run(() => api.clearStoreWarnings(), "")}>閉じる</button></div>}
        {(notice || error) && <div className={error ? "banner error" : "banner"}><span>{error || notice}</span><button onClick={() => { setError(""); setNotice(""); }}>閉じる</button></div>}
        {tab === "dashboard" && <CheckPanel check={check && check.project_id === (snapshot.running_project_id ?? snapshot.active_project_id) ? check : null} error={checkError} onReview={() => setTab("checks")} />}
        {tab === "dashboard" && <Dashboard snapshot={snapshot} activeCore={activeCore?.status ?? "unknown"} onStartCore={() => void run(api.startCore, "Coreコントローラーを起動しました。")} onStopCore={() => void run(api.stopCore, "Coreコントローラーを停止しました。")} onLifecycle={(command) => void run(async () => { const result = await api.runLifecycle(command); if (result.status === "failed") throw new Error(result.stderr || "hako-cmdが失敗しました。"); }, `hako-cmd ${command} を実行しました。`)} onAssetClick={(id) => { setSelectedAssetId(id); setTab("project"); }} onConnectionClick={(id) => { setSelectedConnectionId(id); setTab("topology"); }} />}
        {tab === "project" && <ProjectPage key={workspace.id} snapshot={snapshot} busy={busy} selectedId={selectedAssetId} onSelect={setSelectedAssetId} onRun={run} onOpenInLibrary={(assetId) => { setLibraryFocus(assetId); setTab("library"); }} />}
        {tab === "library" && <LibraryPage key={workspace.id} projectId={snapshot.active_project_id} members={snapshot.members} running={snapshot.running_project_id === snapshot.active_project_id} busy={busy} focusAssetId={libraryFocus} onRun={run} />}
        {tab === "checks" && <ChecksPage check={check && check.project_id === (snapshot.running_project_id ?? snapshot.active_project_id) ? check : null} error={checkError} disabled={busy || !!snapshot.running_project_id} onAccept={(projectId, revisions) => void run(() => api.acceptMemberChanges(projectId, revisions), "変更を承認しました。")} />}
        {tab === "topology" && <TopologyPage key={workspace.id} check={check && check.project_id === (snapshot.running_project_id ?? snapshot.active_project_id) ? check : null} snapshot={snapshot} selected={selectedConnection} onSelect={setSelectedConnectionId} onHeartbeat={(id) => void run(() => api.recordHeartbeat(id, "ユーザー操作による確認イベント"), "通信確認イベントを記録しました。")} />}
        {tab === "core" && <CorePage key={`${workspace.id}:${JSON.stringify(workspace.core_controller ?? null)}`} workspace={workspace} catalog={catalog} activeCore={activeCore?.status ?? "unknown"} busy={busy} onSaveController={(controller) => void run(() => api.setCoreController(snapshot.active_project_id, workspace.core_controller ?? null, controller), "Core設定を保存しました。")} onInstall={(version) => void run(() => api.installCore(version), "Coreを検証して導入しました。")} onStart={() => void run(api.startCore, "Coreコントローラーを起動しました。")} onStop={() => void run(api.stopCore, "Coreコントローラーを停止しました。")} />}
        {tab === "workspace" && <WorkspacePage key={`${workspace.id}:${workspace.name}`} workspace={workspace} busy={busy} preview={preview} onChoose={() => void chooseDirectory()} onImportV1={() => void importV1()} onApply={(mode) => preview && void run(async () => { await api.applyPreview(preview, mode); resetView(); }, mode === "new_project" ? "新しいプロジェクトとして取り込みました。" : "このプロジェクトにメンバーとして足しました。")} onRename={(name) => void run(() => api.renameProject(snapshot.active_project_id, workspace.name, name), "プロジェクト名を保存しました。")} />}
        {tab === "workspace" && <ProjectFilePanel key={workspace.id} projectId={snapshot.active_project_id} projectName={workspace.name} sourceDirectory={workspace.source_directory ?? null} busy={busy} running={!!snapshot.running_project_id} onRun={run} onImported={() => { resetView(); setTab("checks"); }} />}
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

function CorePage({ workspace, catalog, activeCore, busy, onSaveController, onInstall, onStart, onStop }: { workspace: Workspace; catalog: CoreCatalog | null; activeCore: string; busy: boolean; onSaveController: (controller: NonNullable<Workspace["core_controller"]>) => void; onInstall: (version: string) => void; onStart: () => void; onStop: () => void }) {
  const [controller, setController] = useState(workspace.core_controller ?? { id: crypto.randomUUID(), name: "Hakoniwa Core Controller", command: { program: "", args: [], cwd: null, env: {}, target: "native" as const }, readiness: { kind: "manual" as const } });
  const currentVersion = workspace.core_release?.version;
  return <section className="core-page"><div className="panel"><div className="panel-heading"><div><p className="eyebrow">APPROVED CORE RELEASES</p><h2>Coreの導入と切替</h2></div><StatusBadge value={activeCore} /></div>{catalog && catalog.releases.length > 0 ? <div className="release-list">{catalog.releases.map((release) => <div className="release" key={release.version}><div><strong>hakoniwa-core-pro {release.version}</strong><small>revision: {release.source_revision} · 承認済みアーティファクト {release.artifacts.length}件</small></div><div>{currentVersion === release.version ? <span className="installed">選択中</span> : <button className="button primary small" disabled={busy} onClick={() => onInstall(release.version)}>検証して導入</button>}</div></div>)}</div> : <div className="empty-state"><strong>承認済みのOS別バイナリが未登録です。</strong><p>公式リリースには現時点でOS別バイナリ配布の整合性情報が揃っていません。リリースCIで生成した署名・SHA-256付きのアーティファクトを、管理者が承認カタログへ追加してください。</p></div>}<p className="security-note">導入時はHTTPS取得、SHA-256照合、zip-slip防止、アトミック配置を行います。検証に失敗した実行ファイルは利用しません。</p></div><div className="panel"><div className="panel-heading"><div><p className="eyebrow">CORE CONTROLLER</p><h2>実行コントローラー</h2></div><div className="button-row"><button className="button subtle" onClick={onStart}>起動</button><button className="button subtle" onClick={onStop}>停止</button></div></div><p className="muted">例: Conductor所有アセットやLauncherを指定します。<code>hako-cmd start</code>は別途ライフサイクル制御として実行されます。</p><div className="form-grid"><Field label="名称"><input value={controller.name} onChange={(event) => setController({ ...controller, name: event.target.value })} /></Field><Field label="実行ファイル / コマンド" wide><input value={controller.command.program} placeholder="例: /path/to/hako-conductor または python3.12" onChange={(event) => setController({ ...controller, command: { ...controller.command, program: event.target.value } })} /></Field><Field label="引数（1行に1個）" wide><textarea value={controller.command.args.join("\n")} onChange={(event) => setController({ ...controller, command: { ...controller.command, args: event.target.value.split("\n").map((value) => value.trim()).filter(Boolean) } })} /></Field><Field label="作業ディレクトリ" wide><input value={controller.command.cwd ?? ""} onChange={(event) => setController({ ...controller, command: { ...controller.command, cwd: event.target.value || null } })} /></Field></div><div className="form-actions"><button className="button primary" disabled={busy} onClick={() => onSaveController(controller)}>コントローラー設定を保存</button></div></div></section>;
}

function WorkspacePage({ workspace, busy, preview, onChoose, onImportV1, onApply, onRename }: { workspace: Workspace; busy: boolean; preview: ImportPreview | null; onChoose: () => void; onImportV1: () => void; onApply: (mode: "new_project" | "add_to_project") => void; onRename: (name: string) => void }) {
  const [name, setName] = useState(workspace.name);
  return <section className="workspace-page"><div className="panel"><div className="panel-heading"><div><p className="eyebrow">BUSINESS PACK IMPORT</p><h2>設定ディレクトリを取り込む</h2></div><button className="button primary" onClick={onChoose}>ディレクトリを選択</button></div><p className="muted">Launcherのassets、Endpointのtransport、Bridgeの経路を読み取り、新しいプロジェクトか今のプロジェクトに入れます。元ファイルは変更しません。</p>{preview && <div className="import-preview"><div className="metric-row"><Metric label="検出ファイル" value={preview.discovered_files.length} hint="JSON設定" /><Metric label="アセット" value={preview.assets.length} hint="Launcher assets[]" /><Metric label="接続" value={preview.connections.length} hint="Endpoint / Bridge" /><Metric label="注意" value={preview.warnings.length} hint="確認が必要" /></div><div className="warning-list">{preview.warnings.map((warning) => <p key={warning}>{warning}</p>)}</div><div className="form-actions"><button className="button secondary" disabled={busy} onClick={() => onApply("add_to_project")}>このプロジェクトに足す</button><button className="button primary" disabled={busy} onClick={() => onApply("new_project")}>新しいプロジェクトとして取り込む</button></div></div>}</div><div className="panel"><div className="panel-heading"><div><p className="eyebrow">V1 WORKSPACE</p><h2>v1 の workspace.json を取り込む</h2></div><button className="button secondary" onClick={onImportV1}>ファイルを選択</button></div><p className="muted">以前の形式（workspace.json）の構成を、プロジェクトとして取り込みます。同じ出所のプロジェクトは置き換えるか、新しいプロジェクトとして足すかを選べます。</p></div><div className="panel"><div className="panel-heading"><div><p className="eyebrow">PROJECT</p><h2>プロジェクトの設定</h2></div></div><div className="form-grid"><Field label="プロジェクト名" wide><input value={name} onChange={(event) => setName(event.target.value)} /></Field><Field label="取込元ディレクトリ" wide><input value={workspace.source_directory ?? "未設定"} disabled /></Field></div><div className="form-actions"><button className="button primary" disabled={busy} onClick={() => onRename(name)}>保存</button></div></div></section>;
}

function Metric({ label, value, hint }: { label: string; value: string | number; hint: string }) { return <div className="metric"><span>{label}</span><strong>{value}</strong><small>{hint}</small></div>; }
function StatusBadge({ value }: { value: string }) { return <span className="status-badge" data-state={value}>{labelFor(value)}</span>; }
function EventTable({ events }: { events: WorkspaceSnapshot["recent_events"] }) { return <div className="events-table"><h3>通信タイムライン</h3>{events.length === 0 ? <Empty label="この経路のイベントはまだありません。" /> : events.slice(0, 50).map((event) => <div className="event" key={event.id}><span className="event-time">{fmt.format(new Date(event.observed_at))}</span><span className="event-type">{event.event_type}</span><span>{event.direction}</span><code>{event.pdu_name ?? "—"}</code><span>{event.byte_count ?? "—"}</span><span>{event.message}</span></div>)}</div>; }
function labelFor(value: string) { return ({ running: "稼働中", starting: "起動中", stopping: "停止中", exited: "終了", failed: "失敗", connected: "通信中", idle: "待機", disconnected: "切断", unknown: "未確認" } as Record<string, string>)[value] ?? value; }
