import { useEffect, useState } from "react";
import { api } from "./api";
import type { Access, ActivationTiming, AssetRole, LibraryAsset, LibraryEntry, MemberSettings, Variant, VariantPorts } from "./types";
import { Empty, EnvArea, Field, LinesArea, ReadinessEditor, accessLabel, messageOf } from "./ui";

const roles: AssetRole[] = ["simulator", "controller", "visualizer", "bridge", "service", "external_client", "monitor", "other"];

function newVariant(id: string): Variant {
  return { id, label: "既定", launch: { program: "", args: [], cwd: null, env: {}, target: "native" }, activation_timing: "after_start", readiness: { kind: "manual" }, config_files: [], pdu_entries: [], extra_inputs: [], manual_access: [] };
}

function newLibraryAsset(): LibraryAsset {
  return { schema_version: 2, id: "", name: "新しいアセット", role: "other", description: "", root_dir: null, variants: [newVariant("default")], updated_at: new Date().toISOString() };
}

/**
 * ライブラリ: 手元のアセットの実体。保存すると新しい版になり、プロジェクトのメンバーは
 * 選んだものだけが新しい版に付け替わる。
 */
export function LibraryPage({ projectId, members, running, busy, focusAssetId, onRun }: { projectId: string; members: MemberSettings[]; running: boolean; busy: boolean; focusAssetId: string | null; onRun: (action: () => Promise<unknown>, message: string) => Promise<void> }) {
  const [entries, setEntries] = useState<LibraryEntry[] | null>(null);
  const [loadError, setLoadError] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(focusAssetId);
  const [creating, setCreating] = useState(false);

  const load = async () => {
    try {
      setEntries(await api.library());
      setLoadError("");
    } catch (reason) {
      setLoadError(messageOf(reason));
    }
  };
  useEffect(() => { void load(); }, [projectId, members.map((member) => member.asset_id).join()]);
  useEffect(() => { if (focusAssetId) { setSelectedId(focusAssetId); setCreating(false); } }, [focusAssetId]);

  const selected = entries?.find((entry) => entry.asset.id === selectedId) ?? null;
  const save = (asset: LibraryAsset, baseId: string | null, applyMemberIds: string[]) => onRun(async () => {
    try {
      const id = await api.saveLibraryAsset(asset, baseId, applyMemberIds.length > 0 ? projectId : null, applyMemberIds);
      setCreating(false);
      setSelectedId(id);
    } finally {
      // 失敗しても読み直す。アセットだけ書けてプロジェクトの保存に失敗したとき、そのアセットを一覧に出すため。
      await load();
    }
  }, applyMemberIds.length > 0 ? "アセットを保存し、選んだメンバーに反映しました。" : "アセットをライブラリに保存しました。");
  const remove = (id: string) => onRun(async () => { await api.deleteLibraryAsset(id); setSelectedId(null); await load(); }, "アセットをライブラリから消しました。");

  return <section className="split-page">
    <div className="panel asset-list-panel">
      <div className="panel-heading"><div><p className="eyebrow">LIBRARY</p><h2>ライブラリ</h2></div><button className="button primary small" disabled={busy} onClick={() => { setCreating(true); setSelectedId(null); }}>追加</button></div>
      <p className="muted">アセットの実体（起動定義と PDU 設定）。保存すると新しい版になります。プロジェクトへの反映は、メンバーを選んで行います。</p>
      {loadError && <p className="error">{loadError}</p>}
      <div className="asset-cards">
        {(entries ?? []).map((entry) => {
          const usedHere = entry.used_by.find((user) => user.project_id === projectId);
          return <button key={entry.asset.id} className={selectedId === entry.asset.id && !creating ? "asset-card selected" : "asset-card"} onClick={() => { setSelectedId(entry.asset.id); setCreating(false); }}>
            <span className="dot" data-state={usedHere ? "running" : entry.used_by.length > 0 ? "idle" : "unknown"} />
            <strong>{entry.asset.name}</strong>
            <small>{entry.asset.role} · バリエーション {entry.asset.variants.length} · {entry.used_by.length === 0 ? "未参照" : `${entry.used_by.map((user) => user.project_name).join("、")} が使用`}</small>
          </button>;
        })}
        {entries?.length === 0 && <Empty label="ライブラリは空です。" />}
      </div>
    </div>
    <div className="panel editor-panel">
      {creating ? <LibraryEditor key="new" entry={{ asset: newLibraryAsset(), used_by: [] }} isNew projectId={projectId} members={members} running={running} busy={busy} onSave={save} onDelete={() => undefined} />
        : selected ? <LibraryEditor key={selected.asset.id} entry={selected} isNew={false} projectId={projectId} members={members} running={running} busy={busy} onSave={save} onDelete={() => void remove(selected.asset.id)} />
        : <Empty label="左側からアセットを選択してください。" />}
    </div>
  </section>;
}

function LibraryEditor({ entry, isNew, projectId, members, running, busy, onSave, onDelete }: { entry: LibraryEntry; isNew: boolean; projectId: string; members: MemberSettings[]; running: boolean; busy: boolean; onSave: (asset: LibraryAsset, baseId: string | null, applyMemberIds: string[]) => Promise<void>; onDelete: () => void }) {
  const [draft, setDraft] = useState(entry.asset);
  const [variantIndex, setVariantIndex] = useState(0);
  const usersHere = members.filter((member) => member.asset_id === entry.asset.id);
  const [apply, setApply] = useState<string[]>(usersHere.map((member) => member.id));
  const variant = draft.variants[Math.min(variantIndex, draft.variants.length - 1)];
  const patchVariant = (next: Partial<Variant>) => setDraft((current) => ({ ...current, variants: current.variants.map((candidate, index) => index === variantIndex ? { ...candidate, ...next } : candidate) }));
  const patchLaunch = (next: Partial<Variant["launch"]>) => patchVariant({ launch: { ...variant.launch, ...next } });
  const valid = draft.name.trim() !== "" && draft.variants.every((candidate) => candidate.launch.program.trim() !== "" && candidate.id.trim() !== "");
  const addVariant = () => {
    const ids = new Set(draft.variants.map((candidate) => candidate.id));
    let n = draft.variants.length + 1;
    while (ids.has(`variant-${n}`)) n++;
    setDraft((current) => ({ ...current, variants: [...current.variants, { ...structuredClone(variant), id: `variant-${n}`, label: `${variant.label} の複製` }] }));
    setVariantIndex(draft.variants.length);
  };
  const removeVariant = () => { setDraft((current) => ({ ...current, variants: current.variants.filter((_, index) => index !== variantIndex) })); setVariantIndex(0); };
  const variantInUse = (id: string) => members.some((member) => member.asset_id === entry.asset.id && member.variant_id === id);

  return <>
    <div className="panel-heading"><div><p className="eyebrow">LIBRARY ASSET</p><h2>{draft.name}</h2></div>{!isNew && <button className="button danger small" disabled={busy || entry.used_by.length > 0} title={entry.used_by.length > 0 ? "使っているプロジェクトがあるので消せません。" : undefined} onClick={onDelete}>消す</button>}</div>
    <div className="form-grid">
      <Field label="名前"><input value={draft.name} onChange={(event) => setDraft({ ...draft, name: event.target.value })} /></Field>
      <Field label="役割"><select value={draft.role} onChange={(event) => setDraft({ ...draft, role: event.target.value as AssetRole })}>{roles.map((role) => <option key={role}>{role}</option>)}</select></Field>
      <Field label="説明" wide><textarea value={draft.description} onChange={(event) => setDraft({ ...draft, description: event.target.value })} /></Field>
    </div>
    <div className="variant-tabs">
      {draft.variants.map((candidate, index) => <button key={index} className={index === variantIndex ? "nav-item active" : "nav-item"} onClick={() => setVariantIndex(index)}>{candidate.label || candidate.id}</button>)}
      <button className="button subtle small" onClick={addVariant}>バリエーションを複製</button>
      {draft.variants.length > 1 && <button className="button subtle small" disabled={variantInUse(variant.id)} title={variantInUse(variant.id) ? "このプロジェクトのメンバーが使っています。" : undefined} onClick={removeVariant}>このバリエーションを消す</button>}
    </div>
    <div className="form-grid">
      <Field label="バリエーション ID"><input value={variant.id} disabled={!isNew && entry.asset.variants.some((candidate) => candidate.id === variant.id)} onChange={(event) => patchVariant({ id: event.target.value })} /></Field>
      <Field label="表示名"><input value={variant.label} onChange={(event) => patchVariant({ label: event.target.value })} /></Field>
      <Field label="実行ファイル / コマンド" wide><input value={variant.launch.program} placeholder="例: python" onChange={(event) => patchLaunch({ program: event.target.value })} /></Field>
      <Field label="引数（1行に1個）" wide><LinesArea key={`args-${variantIndex}`} trim={false} value={variant.launch.args} onChange={(args) => patchLaunch({ args })} /></Field>
      <Field label="作業ディレクトリ" wide><input value={variant.launch.cwd ?? ""} onChange={(event) => patchLaunch({ cwd: event.target.value || null })} /></Field>
      <Field label="実行環境"><select value={variant.launch.target === "native" ? "native" : "wsl"} onChange={(event) => patchLaunch({ target: event.target.value === "native" ? "native" : { wsl: { distribution: "Ubuntu" } } })}><option value="native">ローカルOS</option><option value="wsl">WSL2</option></select></Field>
      {variant.launch.target !== "native" && <Field label="WSLディストリビューション"><input value={variant.launch.target.wsl.distribution} onChange={(event) => patchLaunch({ target: { wsl: { distribution: event.target.value } } })} /></Field>}
      <Field label="起動タイミング（既定）"><select value={variant.activation_timing} onChange={(event) => patchVariant({ activation_timing: event.target.value as ActivationTiming })}><option value="before_start">before_start</option><option value="manual">manual</option><option value="after_start">after_start</option></select></Field>
      <Field label="準備完了の条件（既定）"><ReadinessEditor value={variant.readiness} onChange={(readiness) => patchVariant({ readiness })} /></Field>
      <Field label="環境変数（KEY=VALUE、1行に1個）" wide><EnvArea key={`env-${variantIndex}`} value={variant.launch.env} onChange={(env) => patchLaunch({ env })} /></Field>
      <Field label="設定ファイル（1行に1個）" wide><LinesArea key={`config-${variantIndex}`} value={variant.config_files} onChange={(config_files) => patchVariant({ config_files })} /><small>PDU 設定の入口を登録していなければ、ここから推定します。</small></Field>
      <Field label="ほかに変更を監視するファイル（1行に1個）" wide><LinesArea key={`extra-${variantIndex}`} value={variant.extra_inputs} onChange={(extra_inputs) => patchVariant({ extra_inputs })} /><small>サイズを直接書いたスクリプトなど。変われば承認を求めます。</small></Field>
    </div>
    <PduEntriesEditor variant={variant} onChange={patchVariant} />
    <PortsView variant={variant} />
    {!isNew && <div className="usage-list"><h3>使っているプロジェクト</h3>{entry.used_by.length === 0 ? <p className="muted">どのプロジェクトも使っていません。</p> : entry.used_by.map((user) => <p key={user.project_id}>{user.project_name}：{user.member_ids.join("、")}</p>)}</div>}
    {usersHere.length > 0 && <div className="apply-list">
      <h3>このプロジェクトに反映するメンバー</h3>
      <p className="muted">選んだメンバーは、保存した版に付け替わります。内容が変わったメンバーは、接続チェックで承認するまで一括起動を止めます。{running ? "実行中は反映できません。" : ""}</p>
      {usersHere.map((member) => <label key={member.id} className="toggle"><input type="checkbox" disabled={running} checked={apply.includes(member.id)} onChange={(event) => setApply((current) => event.target.checked ? [...current, member.id] : current.filter((id) => id !== member.id))} />{member.name ?? member.id}（{member.variant_id}）</label>)}
    </div>}
    <div className="form-actions">
      {!isNew && <button className="button secondary" disabled={busy || !valid} onClick={() => void onSave(draft, entry.asset.id, [])}>ライブラリだけに保存</button>}
      {usersHere.length > 0 && <button className="button primary" disabled={busy || !valid || running || apply.length === 0} onClick={() => void onSave(draft, entry.asset.id, apply)}>保存して選んだメンバーに反映</button>}
      {isNew && <button className="button primary" disabled={busy || !valid} onClick={() => void onSave(draft, null, [])}>ライブラリに追加</button>}
    </div>
  </>;
}

function PduEntriesEditor({ variant, onChange }: { variant: Variant; onChange: (next: Partial<Variant>) => void }) {
  const entries = variant.pdu_entries;
  const setEntry = (index: number, next: Partial<Variant["pdu_entries"][number]>) => onChange({ pdu_entries: entries.map((entry, i) => i === index ? { ...entry, ...next, usage_inferred: false } : entry) });
  const access = variant.manual_access;
  return <div className="sub-editor">
    <h3>PDU 設定の入口</h3>
    <p className="muted">登録 = <code>asset_register</code> に渡してチャネルを作る。接続 = 既にあるチャネルにつなぐ。空なら設定ファイルから推定します。</p>
    {entries.map((entry, index) => <div key={index} className="inline-fields"><input value={entry.path} placeholder="config/pdu_config.json" onChange={(event) => setEntry(index, { path: event.target.value })} /><select value={entry.usage} onChange={(event) => setEntry(index, { usage: event.target.value as "register" | "attach" })}><option value="register">登録</option><option value="attach">接続</option></select>{entry.usage_inferred && <span className="muted">推定</span>}<button className="button subtle small" onClick={() => onChange({ pdu_entries: entries.filter((_, i) => i !== index) })}>外す</button></div>)}
    <button className="button subtle small" onClick={() => onChange({ pdu_entries: [...entries, { path: "", usage: "attach", usage_inferred: false }] })}>入口を足す</button>
    <h3>向きの補足</h3>
    <p className="muted">compact 形式のように向きを宣言していないチャネルの、読み・書きを補います。</p>
    {access.map((item, index) => <div key={index} className="inline-fields"><input value={item.robot} placeholder="robot" onChange={(event) => onChange({ manual_access: access.map((a, i) => i === index ? { ...a, robot: event.target.value } : a) })} /><input type="number" value={item.channel_id} onChange={(event) => onChange({ manual_access: access.map((a, i) => i === index ? { ...a, channel_id: Number(event.target.value) } : a) })} /><select value={item.access} onChange={(event) => onChange({ manual_access: access.map((a, i) => i === index ? { ...a, access: event.target.value as Access } : a) })}><option value="read">読</option><option value="write">書</option></select><button className="button subtle small" onClick={() => onChange({ manual_access: access.filter((_, i) => i !== index) })}>外す</button></div>)}
    <button className="button subtle small" onClick={() => onChange({ manual_access: [...access, { robot: "", channel_id: 0, access: "read" }] })}>補足を足す</button>
  </div>;
}

/** 編集中のバリエーションの PDU 設定から読んだポート。 */
function PortsView({ variant }: { variant: Variant }) {
  const [ports, setPorts] = useState<VariantPorts | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const read = async () => {
    setLoading(true);
    try { setPorts(await api.inspectVariant(variant)); setError(""); } catch (reason) { setPorts(null); setError(messageOf(reason)); } finally { setLoading(false); }
  };
  useEffect(() => { void read(); }, [variant.id]);
  return <div className="sub-editor">
    <div className="panel-heading"><h3>ポート</h3><button className="button subtle small" disabled={loading} onClick={() => void read()}>編集中の内容で読み直す</button></div>
    {error && <p className="error">{error}</p>}
    {ports && <>
      {ports.inferred && ports.entries.length > 0 && <p className="muted">入口は設定ファイルから推定しました：{ports.entries.map((entry) => `${entry.path}（${entry.usage === "register" ? "登録" : "接続"}）`).join("、")}</p>}
      {ports.problems.map((problem) => <p key={problem} className="finding error"><span>問題</span>{problem}</p>)}
      {ports.coverage === "partial" && <p className="muted">RPC の service 定義など、一部の宣言は確かめていません。</p>}
      {ports.channels.length === 0 ? <p className="muted">チャネルの宣言はありません。</p> : <table className="channel-matrix">
        <thead><tr><th>robot</th><th>ch</th><th>名前</th><th>型</th><th>サイズ</th><th>使い方</th></tr></thead>
        <tbody>{ports.channels.map((channel, index) => <tr key={index}><td>{channel.robot}</td><td>{channel.channel_id}</td><td>{channel.org_name}</td><td><code>{channel.type_name}</code></td><td>{channel.pdu_size}</td><td>{[channel.registers ? "登録" : "", ...channel.access.filter((a) => !(channel.registers && a === "unknown")).map((a) => accessLabel[a])].filter(Boolean).join("・")}</td></tr>)}</tbody>
      </table>}
    </>}
  </div>;
}
