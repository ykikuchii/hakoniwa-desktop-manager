import type { ProjectCheck } from "./types";

const severityLabel: Record<string, string> = { error: "エラー", warning: "注意", info: "情報" };

/** 一括起動の前に行う接続チェックの結果と、承認が必要なメンバー。 */
export function CheckPanel({ check, error, disabled, onAccept }: { check: ProjectCheck | null; error: string; disabled: boolean; onAccept: (memberIds: string[]) => void }) {
  if (!check) {
    return <div className="panel check-panel"><div className="panel-heading"><div><p className="eyebrow">PDU CHECK</p><h3>接続チェック</h3></div></div><p className="muted">{error || "確認しています。"}</p></div>;
  }
  const pending = check.members.filter((member) => member.enabled && (member.acceptance === "changed" || member.acceptance === "not_accepted"));
  const findings = check.report.findings.filter((finding) => finding.severity !== "info");
  const infos = check.report.findings.length - findings.length;
  const errors = findings.filter((finding) => finding.severity === "error").length;
  const unchecked = check.members.filter((member) => member.enabled && member.acceptance === "no_pdu_config").map((member) => member.name);
  const ok = errors === 0 && pending.length === 0;
  return (
    <div className="panel check-panel">
      <div className="panel-heading">
        <div><p className="eyebrow">PDU CHECK</p><h3>接続チェック</h3></div>
        <span className="status-badge" data-state={ok ? "running" : "failed"}>{ok ? "一括起動できます" : "一括起動を止めます"}</span>
      </div>
      <p className="muted">一括起動の対象のメンバーについて、PDU 設定の宣言どうしがつながるかを確かめます。スクリプトに直接書かれたサイズまでは確かめません。{check.report.coverage === "partial" ? "RPC の service 定義は確かめていません。" : ""}</p>
      {pending.length > 0 && (
        <div className="check-pending">
          <p><strong>承認が必要な変更</strong></p>
          {pending.map((member) => <p key={member.member_id}>{member.name}：{member.acceptance === "not_accepted" ? "まだ承認されていません" : member.changes.join("、")}</p>)}
          <div className="form-actions"><button className="button primary" disabled={disabled} onClick={() => onAccept(pending.map((member) => member.member_id))}>内容を確認したので承認する</button></div>
        </div>
      )}
      {findings.length === 0 ? <p className="muted">問題は見つかりませんでした{infos > 0 ? `（情報 ${infos} 件）` : ""}。</p> : (
        <div className="check-findings">
          {findings.map((finding, index) => <p key={index} className={`finding ${finding.severity}`}><span>{severityLabel[finding.severity] ?? finding.severity}</span>{finding.message}</p>)}
          {infos > 0 && <p className="muted">ほかに情報 {infos} 件</p>}
        </div>
      )}
      {unchecked.length > 0 && <p className="muted">PDU 設定が見つからず確かめていないメンバー：{unchecked.join("、")}</p>}
    </div>
  );
}
