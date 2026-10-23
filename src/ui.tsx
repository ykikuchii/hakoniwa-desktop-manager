import { useEffect, useState, type ReactNode } from "react";
import type { LaunchContract, ReadinessCheck } from "./types";

export function Field({ label, wide, children }: { label: string; wide?: boolean; children: ReactNode }) { return <label className={wide ? "field wide" : "field"}><span>{label}</span>{children}</label>; }
export function Empty({ label }: { label: string }) { return <div className="empty-state">{label}</div>; }

export function parseEnv(value: string) { return Object.fromEntries(value.split("\n").map((line) => line.trim()).filter(Boolean).map((line) => { const separator = line.indexOf("="); return separator < 0 ? [line, ""] : [line.slice(0, separator).trim(), line.slice(separator + 1)]; })); }
export function formatEnv(env: Record<string, string>) { return Object.entries(env).map(([key, value]) => `${key}=${value}`).join("\n"); }
export function parseLines(value: string) { return value.split("\n").map((line) => line.trim()).filter(Boolean); }
export function messageOf(reason: unknown) { return reason instanceof Error ? reason.message : String(reason); }

function linesOf(text: string, trim: boolean) { return text.split("\n").map((line) => line.replace(/\r$/, "")).map((line) => (trim ? line.trim() : line)).filter((line) => line.trim() !== ""); }

/**
 * 1 行に 1 つの値を書くテキスト欄。入力中の文字列（空行や行末の空白）はそのまま持ち、
 * 外から値が変わったとき（読み直しなど）だけ置き換える。`trim` を外すと前後の空白を残す（引数用）。
 */
export function LinesArea({ value, onChange, trim = true }: { value: string[]; onChange: (value: string[]) => void; trim?: boolean }) {
  const [text, setText] = useState(value.join("\n"));
  const key = JSON.stringify(value);
  useEffect(() => { if (JSON.stringify(linesOf(text, trim)) !== key) setText(value.join("\n")); }, [key]);
  return <textarea value={text} onChange={(event) => { setText(event.target.value); onChange(linesOf(event.target.value, trim)); }} />;
}

/** KEY=VALUE を 1 行に 1 つ書く欄。入力中の文字列は [`LinesArea`] と同じく、そのまま持つ。 */
export function EnvArea({ value, onChange }: { value: Record<string, string>; onChange: (value: Record<string, string>) => void }) {
  const [text, setText] = useState(formatEnv(value));
  const key = JSON.stringify(value);
  useEffect(() => { if (JSON.stringify(parseEnv(text)) !== key) setText(formatEnv(value)); }, [key]);
  return <textarea value={text} onChange={(event) => { setText(event.target.value); onChange(parseEnv(event.target.value)); }} />;
}

/** 承認の対象になる起動の内容。 */
export function ContractView({ contract }: { contract: LaunchContract }) {
  const { launch } = contract;
  const env = Object.entries(launch.env);
  return <dl className="definition">
    <dt>実行ファイル</dt><dd><code>{launch.program}</code></dd>
    {/* 引数は区切りが分かるよう 1 つずつ、空白も見える形（JSON の文字列）で出す。 */}
    <dt>引数</dt><dd>{launch.args.length === 0 ? "—" : launch.args.map((arg, index) => <code key={index} className="arg">{JSON.stringify(arg)}</code>)}</dd>
    <dt>作業ディレクトリ</dt><dd>{launch.cwd ?? "—"}</dd>
    <dt>実行環境</dt><dd>{launch.target === "native" ? "ローカル" : `WSL（${launch.target.wsl.distribution}）`}</dd>
    <dt>環境変数</dt><dd>{env.length === 0 ? "—" : env.map(([key, value]) => <code key={key} className="env-line">{key}={value}</code>)}</dd>
    <dt>起動タイミング</dt><dd>{contract.activation_timing}</dd>
    <dt>準備完了</dt><dd>{describeReadiness(contract.readiness)}</dd>
    <dt>依存先</dt><dd>{[...contract.depends_on, ...contract.optional_depends_on.map((id) => `${id}（任意）`)].join("、") || "—"}</dd>
  </dl>;
}

export const accessLabel: Record<"read" | "write" | "unknown", string> = { read: "読", write: "書", unknown: "?" };

/** 準備完了の条件の編集。 */
export function ReadinessEditor({ value, onChange }: { value: ReadinessCheck; onChange: (value: ReadinessCheck) => void }) {
  return <div className="inline-fields">
    <select value={value.kind} onChange={(event) => onChange(event.target.value === "log_contains" ? { kind: "log_contains", text: "" } : event.target.value === "tcp_port" ? { kind: "tcp_port", host: "127.0.0.1", port: 0 } : { kind: "manual" })}>
      <option value="manual">待たない</option>
      <option value="log_contains">ログに文字列が出るまで</option>
      <option value="tcp_port">TCP ポートが開くまで</option>
    </select>
    {value.kind === "log_contains" && <input value={value.text} placeholder="例: WAIT START" onChange={(event) => onChange({ kind: "log_contains", text: event.target.value })} />}
    {value.kind === "tcp_port" && <><input value={value.host} onChange={(event) => onChange({ ...value, host: event.target.value })} /><input type="number" value={value.port} onChange={(event) => onChange({ ...value, port: Number(event.target.value) })} /></>}
  </div>;
}

export function describeReadiness(value: ReadinessCheck) {
  return value.kind === "manual" ? "待たない" : value.kind === "log_contains" ? `ログに「${value.text}」` : `TCP ${value.host}:${value.port}`;
}
