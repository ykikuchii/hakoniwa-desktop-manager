import { useEffect, useRef, useState } from "react";
import { api } from "./api";
import { selectAssetProcess } from "./selectors";
import type { ActivationTiming, LibraryEntry, MemberSettings, ProjectCheck, WorkspaceSnapshot } from "./types";
import { ContractView, Empty, EnvArea, Field, ReadinessEditor, accessLabel, describeReadiness, messageOf } from "./ui";

type Run = (action: () => Promise<unknown>, message: string) => Promise<void>;

/**
 * プロジェクト: ライブラリのアセットの組み合わせ。メンバーごとに、一括起動の対象・依存・
 * 起動の段取りの上書き・環境変数の上書きを持つ。アセットの起動定義はライブラリで編集する。
 */
export function ProjectPage({ snapshot, busy, selectedId, onSelect, onRun, onOpenInLibrary }: { snapshot: WorkspaceSnapshot; busy: boolean; selectedId: string | null; onSelect: (id: string | null) => void; onRun: Run; onOpenInLibrary: (assetId: string) => void }) {
  const [library, setLibrary] = useState<LibraryEntry[]>([]);
  const [adding, setAdding] = useState(false);
  const projectId = snapshot.active_project_id;
  const running = snapshot.running_project_id === projectId;
  useEffect(() => { api.library().then(setLibrary, () => setLibrary([])); }, [projectId, adding]);
  const selected = snapshot.members.find((member) => member.id === selectedId) ?? null;
  const definitionOf = (id: string) => snapshot.workspace.assets.find((asset) => asset.id === id);

  return <section className="split-page">
    <div className="panel asset-list-panel">
      <div className="panel-heading"><div><p className="eyebrow">PROJECT MEMBERS</p><h2>メンバー</h2></div><button className="button primary small" disabled={busy || running} onClick={() => { setAdding(true); onSelect(null); }}>ライブラリから追加</button></div>
      <p className="muted">ライブラリのアセットを組み合わせます。起動・停止はメンバーごとにできます。</p>
      <div className="asset-cards">
        {snapshot.members.map((member) => {
          const process = selectAssetProcess(snapshot.processes, member.id);
          const definition = definitionOf(member.id);
          return <button key={member.id} className={selectedId === member.id && !adding ? "asset-card selected" : "asset-card"} onClick={() => { setAdding(false); onSelect(member.id); }}>
            <span className="dot" data-state={process?.status ?? "unknown"} />
            <strong>{definition?.name ?? member.name ?? member.id}</strong>
            <small>{member.variant_id} · {definition?.activation_timing ?? ""}{member.enabled ? "" : " · 一括起動の対象外"}</small>
          </button>;
        })}
        {snapshot.members.length === 0 && <Empty label="メンバーがありません。ライブラリから追加してください。" />}
      </div>
    </div>
    <div className="panel editor-panel">
      {adding ? <AddMember key={projectId} projectId={projectId} members={snapshot.members} library={library} busy={busy} onRun={onRun} onDone={(id) => { setAdding(false); onSelect(id); }} />
        : selected ? <MemberEditor key={`${selected.id}:${JSON.stringify(selected)}`} snapshot={snapshot} member={selected} library={library} busy={busy} running={running} onRun={onRun} onOpenInLibrary={onOpenInLibrary} onRemoved={() => onSelect(null)} />
        : <Empty label="左側からメンバーを選択してください。" />}
    </div>
  </section>;
}

function MemberEditor({ snapshot, member, library, busy, running, onRun, onOpenInLibrary, onRemoved }: { snapshot: WorkspaceSnapshot; member: MemberSettings; library: LibraryEntry[]; busy: boolean; running: boolean; onRun: Run; onOpenInLibrary: (assetId: string) => void; onRemoved: () => void }) {
  const [draft, setDraft] = useState(member);
  const [switchTo, setSwitchTo] = useState({ assetId: member.asset_id, variantId: member.variant_id });
  const projectId = snapshot.active_project_id;
  const asset = library.find((entry) => entry.asset.id === member.asset_id)?.asset;
  const variant = asset?.variants.find((candidate) => candidate.id === draft.variant_id);
  const definition = snapshot.workspace.assets.find((candidate) => candidate.id === member.id);
  const others = snapshot.members.filter((candidate) => candidate.id !== member.id);
  const nameOf = (id: string) => snapshot.workspace.assets.find((candidate) => candidate.id === id)?.name ?? id;
  const patch = (next: Partial<MemberSettings>) => setDraft((current) => ({ ...current, ...next }));
  const switchAsset = library.find((entry) => entry.asset.id === switchTo.assetId)?.asset;

  return <>
    <div className="panel-heading"><div><p className="eyebrow">MEMBER</p><h2>{definition?.name ?? member.id}</h2></div><div className="button-row"><button className="button subtle" disabled={busy} onClick={() => void onRun(() => api.startAsset(member.id), "アセットを起動しました。")}>起動</button><button className="button subtle" disabled={busy} onClick={() => void onRun(() => api.stopAsset(member.id), "アセットを停止しました。")}>停止</button></div></div>
    {running && <p className="muted">実行中は変更できません。すべて停止してから変更してください。</p>}
    <div className="form-grid">
      <Field label="メンバー ID"><input value={member.id} disabled /></Field>
      <Field label="表示名（空ならアセットの名前）"><input value={draft.name ?? ""} placeholder={asset?.name} onChange={(event) => patch({ name: event.target.value || null })} /></Field>
      <Field label="バリエーション"><select value={draft.variant_id} onChange={(event) => patch({ variant_id: event.target.value })}>{(asset?.variants ?? [{ id: draft.variant_id, label: draft.variant_id }]).map((candidate) => <option key={candidate.id} value={candidate.id}>{candidate.label}（{candidate.id}）</option>)}</select></Field>
      <Field label="起動タイミング"><select value={draft.activation_timing ?? ""} onChange={(event) => patch({ activation_timing: (event.target.value || null) as ActivationTiming | null })}><option value="">バリエーションの既定（{variant?.activation_timing ?? "?"}）</option><option value="before_start">before_start</option><option value="manual">manual</option><option value="after_start">after_start</option></select></Field>
      <Field label="準備完了の条件"><div><label className="toggle"><input type="checkbox" checked={!draft.readiness} onChange={(event) => patch({ readiness: event.target.checked ? null : variant?.readiness ?? { kind: "manual" } })} />バリエーションの既定（{variant ? describeReadiness(variant.readiness) : "?"}）</label>{draft.readiness && <ReadinessEditor value={draft.readiness} onChange={(readiness) => patch({ readiness })} />}</div></Field>
      <Field label="必須の依存先"><select multiple value={draft.depends_on} onChange={(event) => patch({ depends_on: Array.from(event.target.selectedOptions).map((option) => option.value) })}><option value="core">core</option>{others.map((candidate) => <option key={candidate.id} value={candidate.id}>{nameOf(candidate.id)}</option>)}</select><small>一括起動はこれらの準備完了を待ちます。一括起動の対象外のものを選ぶと、一括起動は拒否されます。</small></Field>
      <Field label="任意の依存先"><select multiple value={draft.optional_depends_on} onChange={(event) => patch({ optional_depends_on: Array.from(event.target.selectedOptions).map((option) => option.value) })}>{others.map((candidate) => <option key={candidate.id} value={candidate.id}>{nameOf(candidate.id)}</option>)}</select><small>一括起動の対象なら待ち、対象外なら待たずに進みます。</small></Field>
      <Field label="環境変数の上書き（KEY=VALUE、1行に1個）" wide><EnvArea value={draft.env_overrides} onChange={(env_overrides) => patch({ env_overrides })} /><small>アセットの環境変数より優先します。HAKO_CONFIG_PATH は Core の設定で指定します。</small></Field>
      <label className="toggle"><input type="checkbox" checked={draft.enabled} onChange={(event) => patch({ enabled: event.target.checked })} />一括起動の対象に含める（外しても単独では起動できます）</label>
    </div>
    <div className="form-actions">
      <button className="button danger" disabled={busy || running} onClick={() => void onRun(async () => { await api.removeMember(projectId, member); onRemoved(); }, "メンバーを外しました。")}>プロジェクトから外す</button>
      <button className="button primary" disabled={busy || running} onClick={() => void onRun(() => api.updateMember(projectId, member, draft), "メンバーを保存しました。")}>保存</button>
    </div>
    <div className="sub-editor">
      <div className="panel-heading"><h3>アセット（ライブラリ）</h3>{asset && <button className="button subtle small" onClick={() => onOpenInLibrary(asset.id)}>ライブラリで編集</button>}</div>
      {definition && <dl className="definition">
        <dt>アセット</dt><dd>{asset?.name ?? member.asset_id}</dd>
        <dt>コマンド</dt><dd><code>{[definition.command.program, ...definition.command.args].join(" ")}</code></dd>
        <dt>作業ディレクトリ</dt><dd>{definition.command.cwd ?? "—"}</dd>
        <dt>実行環境</dt><dd>{definition.command.target === "native" ? "ローカル" : `WSL（${definition.command.target.wsl.distribution}）`}</dd>
        <dt>設定ファイル</dt><dd>{definition.config_files.join("、") || "—"}</dd>
      </dl>}
      <h3>使うアセットを選び直す</h3>
      <div className="inline-fields">
        <select value={switchTo.assetId} onChange={(event) => { const next = library.find((entry) => entry.asset.id === event.target.value)?.asset; setSwitchTo({ assetId: event.target.value, variantId: next?.variants[0]?.id ?? "" }); }}>{library.map((entry) => <option key={entry.asset.id} value={entry.asset.id}>{entry.asset.name}{entry.asset.id === member.asset_id ? "（今の版）" : ""}</option>)}</select>
        <select value={switchTo.variantId} onChange={(event) => setSwitchTo({ ...switchTo, variantId: event.target.value })}>{(switchAsset?.variants ?? []).map((candidate) => <option key={candidate.id} value={candidate.id}>{candidate.label}</option>)}</select>
        <button className="button subtle small" disabled={busy || running || (switchTo.assetId === member.asset_id && switchTo.variantId === member.variant_id)} onClick={() => void onRun(() => api.setMemberAsset(projectId, member.id, member.asset_id, member.variant_id, switchTo.assetId, switchTo.variantId), "使うアセットを選び直しました。接続チェックで承認してください。")}>選び直す</button>
      </div>
    </div>
  </>;
}

function AddMember({ projectId, members, library, busy, onRun, onDone }: { projectId: string; members: MemberSettings[]; library: LibraryEntry[]; busy: boolean; onRun: Run; onDone: (id: string) => void }) {
  const [assetId, setAssetId] = useState("");
  const [variantId, setVariantId] = useState("");
  const [memberId, setMemberId] = useState("");
  const [name, setName] = useState("");
  const [enabled, setEnabled] = useState(true);
  // プレビューは、出したときの設定と一緒に持つ。設定が変わったら、その結果では追加しない。
  const [preview, setPreview] = useState<{ settings: string; check: ProjectCheck } | null>(null);
  const [error, setError] = useState("");
  const generation = useRef(0);
  const asset = library.find((entry) => entry.asset.id === assetId)?.asset;
  const settings: MemberSettings = { id: memberId.trim(), asset_id: assetId, variant_id: variantId, name: name.trim() || null, enabled, depends_on: [], optional_depends_on: [], activation_timing: null, readiness: null, env_overrides: {} };
  const settingsKey = JSON.stringify(settings);
  // 入力を変えたら、前のプレビューも、まだ返ってきていない問い合わせも使わない（表示した内容で承認するため）。
  useEffect(() => { generation.current++; setPreview(null); setError(""); }, [settingsKey]);
  const chooseAsset = (id: string) => {
    const next = library.find((entry) => entry.asset.id === id)?.asset;
    setAssetId(id);
    setVariantId(next?.variants[0]?.id ?? "");
    const base = (next?.name ?? "member").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "member";
    let candidate = base;
    for (let n = 2; members.some((member) => member.id === candidate) || candidate === "core"; n++) candidate = `${base}-${n}`;
    setMemberId(candidate);
  };
  const ready = settings.id !== "" && assetId !== "" && variantId !== "";
  const current = preview && preview.settings === settingsKey ? preview.check : null;
  const shown = current?.members.find((member) => member.member_id === settings.id);
  const related = current?.report.findings.filter((finding) => finding.members.includes(settings.id)) ?? [];
  const requestPreview = () => {
    const mine = ++generation.current;
    const key = settingsKey;
    api.previewAddMember(projectId, settings).then(
      (result) => { if (mine === generation.current) { setPreview({ settings: key, check: result }); setError(""); } },
      (reason) => { if (mine === generation.current) { setPreview(null); setError(messageOf(reason)); } },
    );
  };

  return <>
    <div className="panel-heading"><div><p className="eyebrow">ADD MEMBER</p><h2>ライブラリから追加</h2></div></div>
    <p className="muted">足す前に、足した後の接続チェックを確かめます。表示した内容で承認して足します。</p>
    <div className="form-grid">
      <Field label="アセット"><select value={assetId} onChange={(event) => chooseAsset(event.target.value)}><option value="">選んでください</option>{library.map((entry) => <option key={entry.asset.id} value={entry.asset.id}>{entry.asset.name}</option>)}</select></Field>
      <Field label="バリエーション"><select value={variantId} onChange={(event) => setVariantId(event.target.value)}>{(asset?.variants ?? []).map((candidate) => <option key={candidate.id} value={candidate.id}>{candidate.label}</option>)}</select></Field>
      <Field label="メンバー ID"><input value={memberId} onChange={(event) => setMemberId(event.target.value)} /></Field>
      <Field label="表示名（任意）"><input value={name} placeholder={asset?.name} onChange={(event) => setName(event.target.value)} /></Field>
      <label className="toggle"><input type="checkbox" checked={enabled} onChange={(event) => setEnabled(event.target.checked)} />一括起動の対象に含める</label>
    </div>
    <div className="form-actions">
      <button className="button secondary" disabled={busy || !ready} onClick={requestPreview}>プレビュー</button>
      <button className="button primary" disabled={busy || !shown} onClick={() => void onRun(async () => { await api.addMember(projectId, settings, shown?.revision ?? null); onDone(settings.id); }, "メンバーを追加しました。")}>表示した内容で追加する</button>
    </div>
    {error && <p className="error">{error}</p>}
    {current && shown && <div className="sub-editor">
      <h3>足した後の接続チェック</h3>
      {related.length === 0 ? <p className="muted">このメンバーに関わる所見はありません。</p> : related.map((finding, index) => <p key={index} className={`finding ${finding.severity}`}><span>{({ error: "エラー", warning: "注意", info: "情報" } as Record<string, string>)[finding.severity]}</span>{finding.message}</p>)}
      {current.report.findings.some((finding) => finding.severity === "error") && <p className="muted">エラーがあると、足しても一括起動は止まります。</p>}
      <h3>起動の内容</h3>
      <ContractView contract={shown.contract} />
      <h3>このメンバーのチャネル</h3>
      {shown.channels.length === 0 ? <p className="muted">{shown.files_checked ? "チャネルの宣言はありません。" : "この PC からは PDU 設定を読めません（WSL）。"}</p> : <ul>{shown.channels.map((channel, index) => <li key={index}>{channel.robot} ch{channel.channel_id} {channel.org_name}（<code>{channel.type_name}</code> / {channel.pdu_size}）{[channel.registers ? " 登録" : "", ...channel.access.filter((access) => !(channel.registers && access === "unknown")).map((access) => ` ${accessLabel[access]}`)].join("")}</li>)}</ul>}
    </div>}
  </>;
}
