import { useState } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { api } from "./api";
import type { BundlePreview } from "./types";
import { Field, messageOf } from "./ui";

type Run = (action: () => Promise<unknown>, message: string) => Promise<void>;

/** プロジェクトの書き出し（`.hakoproj.json`）と、書き出したファイルの読み込み。 */
export function ProjectFilePanel({ projectId, projectName, sourceDirectory, busy, running, onRun, onImported }: { projectId: string; projectName: string; sourceDirectory: string | null; busy: boolean; running: boolean; onRun: Run; onImported: () => void }) {
  const [exportRoot, setExportRoot] = useState(sourceDirectory ?? "");
  const [exportWarnings, setExportWarnings] = useState<string[] | null>(null);
  const [path, setPath] = useState<string | null>(null);
  const [importRoot, setImportRoot] = useState("");
  const [preview, setPreview] = useState<BundlePreview | null>(null);
  const [replace, setReplace] = useState(true);
  const [error, setError] = useState("");

  async function exportProject() {
    const target = await save({ title: "プロジェクトを書き出す", defaultPath: `${projectName}.hakoproj.json`, filters: [{ name: "Hakoniwa project", extensions: ["json"] }] });
    if (!target) return;
    await onRun(async () => setExportWarnings(await api.exportProject(projectId, target, exportRoot.trim() || null)), `${target} に書き出しました。`);
  }

  async function inspect(file: string, root: string) {
    setPreview(null);
    setError("");
    try {
      const result = await api.inspectBundle(file, root.trim() || null);
      setPreview(result);
      setReplace(!!result.replaces);
    } catch (reason) {
      setError(messageOf(reason));
    }
  }

  async function chooseFile() {
    const selection = await open({ directory: false, multiple: false, title: "プロジェクトのファイル（.hakoproj.json）を選択", filters: [{ name: "Hakoniwa project", extensions: ["json"] }] });
    if (typeof selection !== "string") return;
    setPath(selection);
    setImportRoot("");
    await inspect(selection, "");
  }

  return <div className="panel">
    <div className="panel-heading"><div><p className="eyebrow">PROJECT FILE</p><h2>プロジェクトの書き出し・読み込み</h2></div></div>
    <p className="muted">プロジェクトと、そのメンバーが使うアセットを 1 つのファイルにします。基準ルートの下のパスは相対表記になり、ファイルと一緒に移せます。承認は書き出しません。</p>
    <div className="form-grid">
      <Field label="基準ルート（空なら取込元のディレクトリ、それもなければ書き出し先）" wide><input value={exportRoot} placeholder={sourceDirectory ?? "例: C:\\work\\hakoniwa"} onChange={(event) => setExportRoot(event.target.value)} /></Field>
    </div>
    <div className="form-actions"><button className="button secondary" disabled={busy} onClick={() => void exportProject()}>このプロジェクトを書き出す</button></div>
    {exportWarnings && (exportWarnings.length === 0 ? <p className="muted">持ち運べないパスは見つかりませんでした。</p> : <div className="warning-list"><p><strong>別の PC では見つからないかもしれないパス</strong></p>{exportWarnings.map((warning) => <p key={warning}>{warning}</p>)}</div>)}
    <div className="sub-editor">
      <div className="panel-heading"><h3>読み込む</h3><button className="button secondary small" disabled={busy || running} title={running ? "実行中は読み込めません。" : undefined} onClick={() => void chooseFile()}>ファイルを選ぶ</button></div>
      {error && <p className="error">{error}</p>}
      {path && preview && <>
        <p><strong>{preview.project_name}</strong>（{path}）</p>
        <p className="muted">メンバー：{preview.members.join("、")}</p>
        <p className="muted">アセット：{preview.assets.map((asset) => `${asset.name}（${asset.reused ? "ライブラリのものを使う" : "ライブラリに足す"}）`).join("、")}</p>
        <div className="inline-fields"><span className="muted">基準ルート</span><input value={importRoot} placeholder={preview.root} onChange={(event) => setImportRoot(event.target.value)} /><button className="button subtle small" disabled={busy} onClick={() => void inspect(path, importRoot)}>この基準でプレビューし直す</button></div>
        {preview.warnings.length > 0 && <div className="warning-list">{preview.warnings.map((warning) => <p key={warning}>{warning}</p>)}</div>}
        {preview.replaces && <label className="toggle"><input type="checkbox" checked={replace} onChange={(event) => setReplace(event.target.checked)} />同じファイルから読み込んだプロジェクト「{preview.replaces}」を置き換える（外すと新しいプロジェクトとして足します）</label>}
        <p className="muted">読み込んだメンバーは承認されていません。接続チェックで起動の内容と PDU の宣言を確かめてから承認してください。</p>
        <div className="form-actions"><button className="button primary" disabled={busy || running} onClick={() => void onRun(async () => { await api.importBundle(path, importRoot.trim() || null, preview.sha256, replace); setPreview(null); setPath(null); onImported(); }, "プロジェクトを読み込みました。接続チェックで承認してください。")}>表示した内容で読み込む</button></div>
      </>}
    </div>
  </div>;
}
