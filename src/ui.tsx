import type { ReactNode } from "react";
import type { ReadinessCheck } from "./types";

export function Field({ label, wide, children }: { label: string; wide?: boolean; children: ReactNode }) { return <label className={wide ? "field wide" : "field"}><span>{label}</span>{children}</label>; }
export function Empty({ label }: { label: string }) { return <div className="empty-state">{label}</div>; }

export function parseEnv(value: string) { return Object.fromEntries(value.split("\n").map((line) => line.trim()).filter(Boolean).map((line) => { const separator = line.indexOf("="); return separator < 0 ? [line, ""] : [line.slice(0, separator).trim(), line.slice(separator + 1)]; })); }
export function formatEnv(env: Record<string, string>) { return Object.entries(env).map(([key, value]) => `${key}=${value}`).join("\n"); }
export function parseLines(value: string) { return value.split("\n").map((line) => line.trim()).filter(Boolean); }
export function messageOf(reason: unknown) { return reason instanceof Error ? reason.message : String(reason); }

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
