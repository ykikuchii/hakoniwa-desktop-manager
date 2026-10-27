import type { ChannelUse, MemberCheck, ProjectCheck } from "./types";
import { ContractView, accessLabel } from "./ui";

const severityLabel: Record<string, string> = { error: "エラー", warning: "注意", info: "情報" };
const acceptanceLabel: Record<MemberCheck["acceptance"], string> = {
  no_pdu_config: "PDU 設定なし",
  accepted: "承認済み",
  changed: "変更あり",
  not_accepted: "未承認",
  unverified: "確かめられない（WSL）",
};

/** 接続チェックの全体: チェック結果、メンバーの承認状態、メンバー × チャネルの対応表。 */
export function ChecksPage({ check, error, disabled, onAccept }: { check: ProjectCheck | null; error: string; disabled: boolean; onAccept: (projectId: string, revisions: Record<string, string>) => void }) {
  if (!check) {
    return <section className="checks-page"><div className="panel"><p className="muted">{error || "確認しています。"}</p></div></section>;
  }
  const pending = check.members.filter((member) => member.enabled && (member.acceptance === "changed" || member.acceptance === "not_accepted"));
  return (
    <section className="checks-page">
      <div className="panel">
        <div className="panel-heading">
          <div><p className="eyebrow">FINDINGS</p><h2>チェック結果</h2></div>
          <span className="muted">{check.report.coverage === "partial" ? "一部の宣言（RPC の service 定義など）は確かめていません" : "宣言はすべて読めました"}</span>
        </div>
        <p className="muted">一括起動の対象のメンバーについて、PDU 設定の宣言どうしを突き合わせます。スクリプトに直接書かれたサイズまでは確かめません。</p>
        {check.report.findings.length === 0 ? <p className="muted">問題は見つかりませんでした。</p> : (
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
              {member.enabled && (member.acceptance === "changed" || member.acceptance === "not_accepted") && <>
                <ContractView contract={member.contract} />
                {member.channels.length > 0 && <p className="muted">チャネル：{member.channels.map((channel) => `${channel.robot} ch${channel.channel_id} ${channel.org_name}（${channel.type_name} / ${channel.pdu_size}${channel.registers ? "・登録" : ""}${channel.access.filter((access) => !(channel.registers && access === "unknown")).map((access) => "・" + accessLabel[access]).join("")}）`).join("、")}</p>}
              </>}
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
  // 行は宣言ごと（robot・ch・名前・型・サイズ）。同じチャネルの宣言が食い違えば、行が分かれて見える。
  const declKey = (channel: ChannelUse) => JSON.stringify([channel.robot, channel.channel_id, channel.org_name, channel.type_name, channel.pdu_size]);
  const channelKey = (channel: ChannelUse) => `${channel.robot}\u0000${channel.channel_id}`;
  const rows = new Map<string, ChannelUse>();
  const owners = new Map<string, ChannelUse>();
  const declarations = new Map<string, Set<string>>();
  for (const member of members) {
    for (const channel of member.channels) {
      rows.set(declKey(channel), channel);
      if (channel.registers && !owners.has(channelKey(channel))) owners.set(channelKey(channel), channel);
      declarations.set(channelKey(channel), (declarations.get(channelKey(channel)) ?? new Set()).add(declKey(channel)));
    }
  }
  const sorted = [...rows.entries()].sort(([, a], [, b]) => a.robot.localeCompare(b.robot) || a.channel_id - b.channel_id || a.org_name.localeCompare(b.org_name) || a.pdu_size - b.pdu_size);
  return (
    <div className="matrix-scroll">
      <table className="channel-matrix">
        <thead><tr><th>robot</th><th>ch</th><th>名前</th><th>型</th><th>サイズ</th>{members.map((member) => <th key={member.member_id}>{member.name}</th>)}</tr></thead>
        <tbody>
          {sorted.map(([key, row]) => {
            const owner = owners.get(channelKey(row));
            const conflicting = (declarations.get(channelKey(row))?.size ?? 0) > 1;
            const mismatch = owner ? row.pdu_size !== owner.pdu_size || row.type_name !== owner.type_name : false;
            return <tr key={key} className={[owner ? "" : "no-owner", conflicting ? "conflict" : ""].join(" ").trim() || undefined}>
              <td>{row.robot}</td><td>{row.channel_id}</td><td>{row.org_name}</td><td><code>{row.type_name}</code></td><td>{row.pdu_size}</td>
              {members.map((member) => {
                const use = member.channels.find((channel) => declKey(channel) === key);
                if (!use) return <td key={member.member_id} />;
                const label = [use.registers ? "登録" : "", ...use.access.filter((access) => !(use.registers && access === "unknown")).map((access) => accessLabel[access])].filter(Boolean).join("・");
                return <td key={member.member_id} className={mismatch ? "cell mismatch" : "cell"} title={mismatch ? `所有側は ${owner?.type_name} / ${owner?.pdu_size}` : undefined}>{label}</td>;
              })}
            </tr>;
          })}
        </tbody>
      </table>
      {sorted.some(([, row]) => (declarations.get(channelKey(row))?.size ?? 0) > 1) && <p className="muted">左端が黄色の行は、同じチャネルに食い違う宣言があることを示します。</p>}
    </div>
  );
}
