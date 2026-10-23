import type { ChannelUse, MemberCheck, ProjectCheck } from "./types";

const severityLabel: Record<string, string> = { error: "エラー", warning: "注意", info: "情報" };
const acceptanceLabel: Record<MemberCheck["acceptance"], string> = {
  no_pdu_config: "PDU 設定なし",
  accepted: "承認済み",
  changed: "変更あり",
  not_accepted: "未承認",
  unverified: "確かめられない（WSL）",
};

/** 接続チェックの全体: 所見、メンバーの承認状態、メンバー × チャネルの対応表。 */
export function ChecksPage({ check, error, disabled, onAccept }: { check: ProjectCheck | null; error: string; disabled: boolean; onAccept: (projectId: string, revisions: Record<string, string>) => void }) {
  if (!check) {
    return <section className="checks-page"><div className="panel"><p className="muted">{error || "確認しています。"}</p></div></section>;
  }
  const pending = check.members.filter((member) => member.enabled && (member.acceptance === "changed" || member.acceptance === "not_accepted"));
  return (
    <section className="checks-page">
      <div className="panel">
        <div className="panel-heading">
          <div><p className="eyebrow">FINDINGS</p><h2>所見</h2></div>
          <span className="muted">{check.report.coverage === "partial" ? "一部の宣言（RPC の service 定義など）は確かめていません" : "宣言はすべて読めました"}</span>
        </div>
        <p className="muted">一括起動の対象のメンバーについて、PDU 設定の宣言どうしを突き合わせます。スクリプトに直接書かれたサイズまでは確かめません。</p>
        {check.report.findings.length === 0 ? <p className="muted">所見はありません。</p> : (
          <div className="check-findings">
            {check.report.findings.map((finding, index) => <p key={index} className={`finding ${finding.severity}`}><span>{severityLabel[finding.severity] ?? finding.severity}</span>{finding.message}</p>)}
          </div>
        )}
      </div>
      <div className="panel">
        <div className="panel-heading">
          <div><p className="eyebrow">ACCEPTANCE</p><h2>メンバーの承認</h2></div>
          {pending.length > 0 && <button className="button primary" disabled={disabled} onClick={() => onAccept(check.project_id, Object.fromEntries(pending.map((member) => [member.member_id, member.revision])))}>表示中の変更を承認する</button>}
        </div>
        <div className="member-checks">
          {check.members.map((member) => (
            <div key={member.member_id} className="member-check" data-state={member.acceptance}>
              <div className="member-check-heading"><strong>{member.name}</strong><span className="status-badge" data-state={member.acceptance === "changed" || member.acceptance === "not_accepted" ? "failed" : member.acceptance === "accepted" ? "running" : "unknown"}>{acceptanceLabel[member.acceptance]}</span>{!member.enabled && <span className="muted">一括起動の対象外</span>}</div>
              {member.entries.length > 0 && <p className="muted">{member.entries.map((entry) => `${entry.path}（${entry.usage === "register" ? "登録" : "接続"}${entry.usage_inferred ? "・推定" : ""}）`).join("、")}</p>}
              {member.acceptance === "changed" && <ul>{member.changes.map((change) => <li key={change}>{change}</li>)}{member.port_changes.map((change) => <li key={change}>{change}</li>)}</ul>}
            </div>
          ))}
        </div>
      </div>
      <div className="panel">
        <div className="panel-heading"><div><p className="eyebrow">CHANNELS</p><h2>メンバー × チャネル</h2></div><span className="muted">登録 = チャネルを作る、読 / 書 = 宣言された向き、? = 向きの宣言なし</span></div>
        <ChannelMatrix members={check.members.filter((member) => member.enabled && member.channels.length > 0)} />
      </div>
    </section>
  );
}

function ChannelMatrix({ members }: { members: MemberCheck[] }) {
  if (members.length === 0) return <p className="muted">チャネルを宣言しているメンバーがありません。</p>;
  const rows = new Map<string, { robot: string; channel_id: number; org_name: string; type_name: string; owner?: ChannelUse }>();
  for (const member of members) {
    for (const channel of member.channels) {
      const key = `${channel.robot}\u0000${channel.channel_id}`;
      const row = rows.get(key) ?? { robot: channel.robot, channel_id: channel.channel_id, org_name: channel.org_name, type_name: channel.type_name };
      if (channel.registers && !row.owner) row.owner = channel;
      rows.set(key, row);
    }
  }
  const sorted = [...rows.entries()].sort(([, a], [, b]) => a.robot.localeCompare(b.robot) || a.channel_id - b.channel_id);
  return (
    <div className="matrix-scroll">
      <table className="channel-matrix">
        <thead><tr><th>robot</th><th>ch</th><th>名前</th><th>型</th>{members.map((member) => <th key={member.member_id}>{member.name}</th>)}</tr></thead>
        <tbody>
          {sorted.map(([key, row]) => (
            <tr key={key} className={row.owner ? undefined : "no-owner"}>
              <td>{row.robot}</td><td>{row.channel_id}</td><td>{row.org_name}</td><td><code>{row.type_name}</code></td>
              {members.map((member) => {
                const use = member.channels.find((channel) => channel.robot === row.robot && channel.channel_id === row.channel_id);
                if (!use) return <td key={member.member_id} />;
                const mismatch = row.owner ? use.pdu_size !== row.owner.pdu_size || use.type_name !== row.owner.type_name : false;
                const label = [use.registers ? "登録" : "", ...use.access.filter((access) => !(use.registers && access === "unknown")).map((access) => ({ read: "読", write: "書", unknown: "?" })[access])].filter(Boolean).join("・");
                return <td key={member.member_id} className={mismatch ? "cell mismatch" : "cell"} title={mismatch ? `所有側は ${row.owner?.type_name} / ${row.owner?.pdu_size}` : undefined}>{label} <small>{use.pdu_size}</small></td>;
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
