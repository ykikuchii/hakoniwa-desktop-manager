//! プロジェクトのメンバーのポートを突き合わせ、PDU でつながるかを判定する。
//!
//! 判定するのは「宣言上の」互換性で、プログラムに直接書かれたサイズなどは
//! 見ない。プロジェクトは閉じている（ほかの場所でチャネルを作るアセットはない）
//! 前提で、読み・書きするチャネルはメンバーのどれかが登録していなければならない。

// 呼び出し側（プロジェクトの resolve と一括起動前の確認）は次の段階で入る。
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ports::{Access, ChannelDecl, Coverage, PortSet};
use crate::types::ActivationTiming;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// 一括起動を止める。
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub code: String,
    pub members: Vec<String>,
    pub robot: Option<String>,
    pub channel_id: Option<i64>,
    pub message: String,
}

/// 判定するメンバー 1 つ分。一括起動の対象（有効なメンバー）だけを渡す。
#[derive(Debug, Clone)]
pub struct MemberPorts {
    pub member_id: String,
    pub name: String,
    pub activation_timing: ActivationTiming,
    /// メンバーの起動環境の `HAKO_CONFIG_PATH`。
    pub hako_config_path: Option<String>,
    pub ports: PortSet,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompatReport {
    pub findings: Vec<Finding>,
    pub coverage: Coverage,
}

impl CompatReport {
    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(|finding| finding.severity == Severity::Error)
    }
}

type Key = (String, i64);

/// `project_hako_config_path` は、プロジェクトの Core 環境の `HAKO_CONFIG_PATH`。
pub fn check(members: &[MemberPorts], project_hako_config_path: Option<&str>) -> CompatReport {
    let mut findings = Vec::new();
    let mut push = |severity, code: &str, members: Vec<String>, channel: Option<&Key>, message: String| {
        findings.push(Finding { severity, code: code.to_owned(), members, robot: channel.map(|key| key.0.clone()), channel_id: channel.map(|key| key.1), message });
    };

    for member in members {
        for problem in &member.ports.problems {
            push(Severity::Error, "pdu_file_unreadable", vec![member.member_id.clone()], None, format!("{}: {problem}", member.name));
        }
        if !member.ports.registrations.is_empty() && member.activation_timing != ActivationTiming::BeforeStart {
            push(
                Severity::Error,
                "late_registration",
                vec![member.member_id.clone()],
                None,
                format!("{} はチャネルを登録しますが、起動タイミングが Core の開始前（before_start）ではありません。Core は開始後の登録を受け付けません。", member.name),
            );
        }
        if let Some(project) = project_hako_config_path {
            match member.hako_config_path.as_deref() {
                Some(own) if !same_path(own, project) => push(
                    Severity::Error,
                    "core_namespace_mismatch",
                    vec![member.member_id.clone()],
                    None,
                    format!("{} の HAKO_CONFIG_PATH（{own}）がプロジェクトの Core（{project}）と違います。別の共有メモリを使うため、つながりません。", member.name),
                ),
                None if !member.ports.access.is_empty() => push(
                    Severity::Warning,
                    "core_namespace_unset",
                    vec![member.member_id.clone()],
                    None,
                    format!("{} に HAKO_CONFIG_PATH が設定されていません。アプリの環境を引き継ぐため、プロジェクトの Core とつながらない可能性があります。", member.name),
                ),
                _ => {}
            }
        }
    }

    // 宣言をチャネルごとに集める。
    let mut owners: BTreeMap<Key, Vec<(&MemberPorts, &ChannelDecl)>> = BTreeMap::new();
    let mut users: BTreeMap<Key, Vec<(&MemberPorts, &ChannelDecl, Access)>> = BTreeMap::new();
    for member in members {
        for decl in &member.ports.registrations {
            owners.entry(decl.key()).or_default().push((member, decl));
        }
        for access in &member.ports.access {
            users.entry(access.channel.key()).or_default().push((member, &access.channel, access.access));
        }
    }

    // 同じチャネルの宣言どうしの矛盾。1 つのメンバーの中の矛盾も含める。
    let all_keys: BTreeSet<&Key> = owners.keys().chain(users.keys()).collect();
    for key in &all_keys {
        let decls: Vec<(&MemberPorts, &ChannelDecl)> = owners.get(*key).into_iter().flatten().copied().chain(users.get(*key).into_iter().flatten().map(|(m, d, _)| (*m, *d))).collect();
        let types: BTreeSet<&str> = decls.iter().map(|(_, d)| d.type_name.as_str()).collect();
        if types.len() > 1 {
            push(Severity::Error, "type_mismatch", member_ids(decls.iter().map(|(m, _)| *m)), Some(key), format!("{}: 型の宣言が食い違っています（{}）", label(key), join(&types)));
        }
        let owner_sizes: BTreeSet<i64> = owners.get(*key).into_iter().flatten().map(|(_, d)| d.pdu_size).collect();
        if owner_sizes.len() > 1 {
            push(
                Severity::Error,
                "owner_size_mismatch",
                member_ids(owners[*key].iter().map(|(m, _)| *m)),
                Some(key),
                format!("{}: 登録側の pdu_size が食い違っています（{}）。先に登録したものが使われます。", label(key), join(&owner_sizes)),
            );
        }
        let names: BTreeSet<&str> = decls.iter().map(|(_, d)| d.org_name.as_str()).collect();
        if names.len() > 1 {
            push(Severity::Warning, "org_name_mismatch", member_ids(decls.iter().map(|(m, _)| *m)), Some(key), format!("{}: 名前の宣言が食い違っています（{}）", label(key), join(&names)));
        }
    }

    // 同じ robot の中で、1 つの名前が複数のチャネルを指している。名前の引き当ては
    // 設定を読むメンバーごとなので、1 つのメンバーの中で起きれば Error、メンバーを
    // またいで起きるだけなら（それぞれは一意に引ける）Warning にする。
    let mut by_name: BTreeMap<(String, String), BTreeMap<&str, BTreeSet<i64>>> = BTreeMap::new();
    for member in members {
        let decls = member.ports.registrations.iter().chain(member.ports.access.iter().map(|access| &access.channel));
        for decl in decls {
            by_name.entry((decl.robot.clone(), decl.org_name.clone())).or_default().entry(member.member_id.as_str()).or_default().insert(decl.channel_id);
        }
    }
    for ((robot, name), per_member) in &by_name {
        for (member_id, channels) in per_member {
            if channels.len() > 1 {
                push(Severity::Error, "ambiguous_name", vec![(*member_id).to_owned()], None, format!("{robot}/{name}: 同じ名前が複数のチャネル（{}）を指しています", join(channels)));
            }
        }
        let channels: BTreeSet<i64> = per_member.values().flatten().copied().collect();
        if channels.len() > 1 && per_member.values().all(|own| own.len() == 1) {
            push(
                Severity::Warning,
                "name_collision",
                per_member.keys().map(|id| (*id).to_owned()).collect(),
                None,
                format!("{robot}/{name}: メンバーによって、同じ名前が別のチャネル（{}）を指しています", join(&channels)),
            );
        }
    }

    // 所有と利用。
    for (key, uses) in &users {
        let Some(owned) = owners.get(key) else {
            push(Severity::Error, "no_owner", member_ids(uses.iter().map(|(m, _, _)| *m)), Some(key), format!("{}: どのメンバーもこのチャネルを登録していません", label(key)));
            continue;
        };
        let owner_size = owned.iter().map(|(_, d)| d.pdu_size).min().unwrap_or_default();
        // 登録しているメンバーが別の設定で読み書きする場合も比べる。ただし登録そのものと
        // 同じ宣言（compact の所有者など）は、上の owner_size_mismatch で見ている。
        // 読みと書きの両方に同じ宣言があっても、1 件にまとめる。
        let mut sized: BTreeSet<(&str, i64)> = BTreeSet::new();
        for (member, decl, _) in uses {
            let registered_as_is = owned.iter().any(|(m, d)| m.member_id == member.member_id && d.pdu_size == decl.pdu_size);
            if registered_as_is || !sized.insert((member.member_id.as_str(), decl.pdu_size)) {
                continue;
            }
            if decl.pdu_size > owner_size {
                push(
                    Severity::Error,
                    "size_exceeds_owner",
                    vec![member.member_id.clone()],
                    Some(key),
                    format!("{}: {} の pdu_size {} が登録側の {owner_size} より大きく、読み書きに失敗します", label(key), member.name, decl.pdu_size),
                );
            } else if decl.pdu_size < owner_size {
                push(
                    Severity::Warning,
                    "size_below_owner",
                    vec![member.member_id.clone()],
                    Some(key),
                    format!("{}: {} の pdu_size {} が登録側の {owner_size} より小さい", label(key), member.name, decl.pdu_size),
                );
            }
        }
        let writers: BTreeSet<&str> = uses.iter().filter(|(_, _, a)| *a == Access::Write).map(|(m, _, _)| m.member_id.as_str()).collect();
        let maybe_writers = uses.iter().any(|(_, _, a)| *a == Access::Unknown);
        if uses.iter().any(|(_, _, a)| *a == Access::Read) && writers.is_empty() && !maybe_writers {
            push(Severity::Warning, "no_writer", member_ids(uses.iter().map(|(m, _, _)| *m)), Some(key), format!("{}: 読むメンバーはいますが、書くメンバーがいません", label(key)));
        }
        if writers.len() > 1 {
            push(Severity::Warning, "multiple_writers", writers.iter().map(|id| (*id).to_owned()).collect(), Some(key), format!("{}: 書くメンバーが複数あります", label(key)));
        }
    }

    // 同じ robot を複数のメンバーが登録している。Core は拒否しないが、所有者を 1 つに
    // する方針（single owner invariant）から外れる。
    let mut robot_owners: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (key, owned) in &owners {
        for (member, _) in owned {
            robot_owners.entry(key.0.as_str()).or_default().insert(member.member_id.as_str());
        }
    }
    for (robot, ids) in &robot_owners {
        if ids.len() > 1 {
            push(Severity::Warning, "shared_robot_registration", ids.iter().map(|id| (*id).to_owned()).collect(), None, format!("{robot}: 複数のメンバーが登録しています"));
        }
    }

    // ほかのメンバーが使わない所有チャネル。robot ごとにまとめる。
    let mut unused: BTreeMap<&str, (Vec<i64>, BTreeSet<&str>)> = BTreeMap::new();
    for (key, owned) in &owners {
        let owner_ids: BTreeSet<&str> = owned.iter().map(|(m, _)| m.member_id.as_str()).collect();
        let used_by_others = users.get(key).is_some_and(|uses| uses.iter().any(|(m, _, _)| !owner_ids.contains(m.member_id.as_str())));
        if !used_by_others {
            let entry = unused.entry(key.0.as_str()).or_default();
            entry.0.push(key.1);
            entry.1.extend(owner_ids);
        }
    }
    for (robot, (channels, ids)) in &unused {
        push(Severity::Info, "unused_channel", ids.iter().map(|id| (*id).to_owned()).collect(), None, format!("{robot}: ほかのメンバーが使わないチャネル（{}）", join(channels)));
    }

    let coverage = members.iter().map(|member| member.ports.coverage).max().unwrap_or(Coverage::Complete);
    if coverage == Coverage::Partial {
        let partial: Vec<String> = members.iter().filter(|m| m.ports.coverage == Coverage::Partial).map(|m| m.member_id.clone()).collect();
        push(Severity::Info, "partial_coverage", partial, None, "RPC の service 定義は検証の対象外です".to_owned());
    }

    findings.sort_by(|a, b| a.severity.cmp(&b.severity).then_with(|| a.code.cmp(&b.code)));
    CompatReport { findings, coverage }
}

/// 2 つのパスが同じファイルを指すか。表記の違い（区切り文字、`.`・`..`、Windows の
/// 大文字小文字）を吸収して比べる。シンボリックリンクまでは解決しない。
fn same_path(a: &str, b: &str) -> bool {
    normalize_path(a) == normalize_path(b)
}

fn normalize_path(path: &str) -> String {
    let unified = path.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for part in unified.split('/') {
        match part {
            "" | "." if !parts.is_empty() => {}
            ".." if parts.last().is_some_and(|last| *last != ".." && !last.ends_with(':') && !last.is_empty()) => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if cfg!(windows) {
        joined.to_lowercase()
    } else {
        joined
    }
}

fn member_ids<'a>(members: impl Iterator<Item = &'a MemberPorts>) -> Vec<String> {
    members.map(|member| member.member_id.clone()).collect::<BTreeSet<_>>().into_iter().collect()
}

fn label(key: &Key) -> String {
    format!("{} ch{}", key.0, key.1)
}

fn join<T: std::fmt::Display>(items: impl IntoIterator<Item = T>) -> String {
    items.into_iter().map(|item| item.to_string()).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::tests::{compact_owner, legacy, scratch, write};
    use crate::ports::{extract, PduEntry, Usage};
    use serde_json::json;
    use std::path::PathBuf;

    const CONFIG: &str = "C:/hako/cpp_core_config.json";

    fn member(id: &str, timing: ActivationTiming, entries: Vec<(PathBuf, Usage)>) -> MemberPorts {
        let entries: Vec<PduEntry> = entries.into_iter().map(|(path, usage)| PduEntry { path, usage }).collect();
        MemberPorts { member_id: id.to_owned(), name: id.to_owned(), activation_timing: timing, hako_config_path: Some(CONFIG.to_owned()), ports: extract(&entries) }
    }

    fn codes(report: &CompatReport, severity: Severity) -> Vec<String> {
        report.findings.iter().filter(|f| f.severity == severity).map(|f| f.code.clone()).collect()
    }

    /// 所有者 1 つ、書き手 1 つ、読み手 1 つの正しい構成は Error も Warning も出ない。
    #[test]
    fn a_consistent_project_has_no_errors_or_warnings() {
        let dir = scratch("compat-ok");
        let owner = member("owner", ActivationTiming::BeforeStart, vec![(compact_owner(&dir, "Rig", &[(0, "cmd", 136), (1, "attitude", 72)]), Usage::Register)]);
        let sim = member("sim", ActivationTiming::AfterStart, vec![(legacy(&dir, "sim.json", "Rig", &[(0, "cmd", 136)], &[(1, "attitude", 72)]), Usage::Attach)]);
        let pilot = member("pilot", ActivationTiming::AfterStart, vec![(legacy(&dir, "pilot.json", "Rig", &[(1, "attitude", 72)], &[(0, "cmd", 136)]), Usage::Attach)]);
        let report = check(&[owner, sim, pilot], Some(CONFIG));
        assert!(codes(&report, Severity::Error).is_empty() && codes(&report, Severity::Warning).is_empty(), "{:#?}", report.findings);
    }

    /// 登録者がいないチャネルを使うと Error。
    #[test]
    fn using_a_channel_nobody_registers_is_an_error() {
        let dir = scratch("compat-no-owner");
        let sim = member("sim", ActivationTiming::AfterStart, vec![(legacy(&dir, "sim.json", "Rig", &[(0, "cmd", 136)], &[]), Usage::Attach)]);
        assert_eq!(codes(&check(&[sim], Some(CONFIG)), Severity::Error), vec!["no_owner"]);
    }

    /// 登録側より大きいサイズは Error、小さいサイズは Warning。
    #[test]
    fn size_larger_than_owner_is_an_error_and_smaller_a_warning() {
        let dir = scratch("compat-size");
        let owner = || member("owner", ActivationTiming::BeforeStart, vec![(compact_owner(&dir, "Rig", &[(0, "cmd", 136)]), Usage::Register)]);
        let big = member("big", ActivationTiming::AfterStart, vec![(legacy(&dir, "big.json", "Rig", &[(0, "cmd", 144)], &[]), Usage::Attach)]);
        assert_eq!(codes(&check(&[owner(), big], Some(CONFIG)), Severity::Error), vec!["size_exceeds_owner"]);
        let small = member("small", ActivationTiming::AfterStart, vec![(legacy(&dir, "small.json", "Rig", &[(0, "cmd", 128)], &[]), Usage::Attach)]);
        let report = check(&[owner(), small], Some(CONFIG));
        assert!(codes(&report, Severity::Error).is_empty());
        assert_eq!(codes(&report, Severity::Warning), vec!["size_below_owner"]);
    }

    /// 型の食い違いは Error。
    #[test]
    fn type_mismatch_is_an_error() {
        let dir = scratch("compat-type");
        let owner = member("owner", ActivationTiming::BeforeStart, vec![(compact_owner(&dir, "Rig", &[(0, "cmd", 136)]), Usage::Register)]);
        let path = write(&dir, "other.json", json!({ "robots": [{ "name": "Rig", "shm_pdu_readers": [{ "type": "std_msgs/String", "org_name": "cmd", "channel_id": 0, "pdu_size": 136 }] }] }));
        let other = member("other", ActivationTiming::AfterStart, vec![(path, Usage::Attach)]);
        assert_eq!(codes(&check(&[owner, other], Some(CONFIG)), Severity::Error), vec!["type_mismatch"]);
    }

    /// 1 つのメンバーの中の矛盾も、黙って重複除去せずに Error にする。
    #[test]
    fn conflicting_declarations_inside_one_member_are_errors() {
        let dir = scratch("compat-self");
        let path = legacy(&dir, "self.json", "Rig", &[(0, "cmd", 136)], &[(0, "cmd", 144)]);
        let owner = member("owner", ActivationTiming::BeforeStart, vec![(path, Usage::Register)]);
        assert_eq!(codes(&check(&[owner], Some(CONFIG)), Severity::Error), vec!["owner_size_mismatch"]);
    }

    /// 1 つのメンバーの中で、1 つの名前が 2 つのチャネルを指すと Error。
    #[test]
    fn a_name_mapped_to_two_channels_in_one_member_is_an_error() {
        let dir = scratch("compat-name");
        let owner = member("owner", ActivationTiming::BeforeStart, vec![(compact_owner(&dir, "Rig", &[(0, "cmd", 136), (1, "cmd", 136)]), Usage::Register)]);
        let report = check(&[owner], Some(CONFIG));
        assert_eq!(codes(&report, Severity::Error), vec!["ambiguous_name"]);
        assert_eq!(report.findings[0].members, vec!["owner"]);
    }

    /// メンバーをまたいで名前の指す先が違うだけなら Warning（それぞれは一意に引ける）。
    #[test]
    fn a_name_differing_across_members_is_a_warning() {
        let dir = scratch("compat-name-cross");
        let owner = member("owner", ActivationTiming::BeforeStart, vec![(compact_owner(&dir, "Rig", &[(0, "cmd", 136), (1, "attitude", 136)]), Usage::Register)]);
        let sim = member("sim", ActivationTiming::AfterStart, vec![(legacy(&dir, "sim.json", "Rig", &[(1, "cmd", 136)], &[]), Usage::Attach)]);
        let report = check(&[owner, sim], Some(CONFIG));
        assert!(!report.has_errors(), "{:#?}", report.findings);
        assert!(codes(&report, Severity::Warning).contains(&"name_collision".to_owned()));
    }

    /// 開始後に起動するメンバーが登録しようとすると Error。
    #[test]
    fn registering_after_core_start_is_an_error() {
        let dir = scratch("compat-late");
        let owner = member("owner", ActivationTiming::AfterStart, vec![(compact_owner(&dir, "Rig", &[(0, "cmd", 136)]), Usage::Register)]);
        assert_eq!(codes(&check(&[owner], Some(CONFIG)), Severity::Error), vec!["late_registration"]);
    }

    /// 別の HAKO_CONFIG_PATH は Error、未設定は Warning。
    #[test]
    fn a_different_core_namespace_is_an_error() {
        let dir = scratch("compat-ns");
        let mut owner = member("owner", ActivationTiming::BeforeStart, vec![(compact_owner(&dir, "Rig", &[(0, "cmd", 136)]), Usage::Register)]);
        owner.hako_config_path = Some("D:/other/cpp_core_config.json".to_owned());
        assert_eq!(codes(&check(&[owner.clone()], Some(CONFIG)), Severity::Error), vec!["core_namespace_mismatch"]);
        owner.hako_config_path = None;
        let report = check(&[owner], Some(CONFIG));
        assert!(codes(&report, Severity::Error).is_empty());
        assert_eq!(codes(&report, Severity::Warning), vec!["core_namespace_unset"]);
    }

    /// 表記が違うだけの同じパスは、同じ Core として扱う。
    #[test]
    fn equivalent_config_paths_are_the_same_namespace() {
        assert!(same_path("C:/hako/cpp_core_config.json", "C:\\hako\\.\\sub\\..\\cpp_core_config.json"));
        assert!(!same_path("C:/hako/cpp_core_config.json", "C:/other/cpp_core_config.json"));
        assert!(!same_path("../a.json", "a.json"));
        let dir = scratch("compat-ns-equal");
        let mut owner = member("owner", ActivationTiming::BeforeStart, vec![(compact_owner(&dir, "Rig", &[(0, "cmd", 136)]), Usage::Register)]);
        owner.hako_config_path = Some("C:\\hako\\.\\cpp_core_config.json".to_owned());
        assert!(!check(&[owner], Some(CONFIG)).has_errors());
    }

    /// 登録するメンバーが、別の設定で登録より大きいサイズを使うのも Error。
    /// 読みと書きの両方にあっても 1 件にまとめる。
    #[test]
    fn a_registrant_attaching_with_a_larger_size_is_an_error_reported_once() {
        let dir = scratch("compat-self-attach");
        let both = legacy(&dir, "both.json", "Rig", &[(0, "cmd", 144)], &[(0, "cmd", 144)]);
        let owner = member("owner", ActivationTiming::BeforeStart, vec![(compact_owner(&dir, "Rig", &[(0, "cmd", 136)]), Usage::Register), (both, Usage::Attach)]);
        assert_eq!(codes(&check(&[owner], Some(CONFIG)), Severity::Error), vec!["size_exceeds_owner"]);
    }

    /// 読めない設定は Error（空のポートとして通さない）。
    #[test]
    fn unreadable_configs_are_errors() {
        let dir = scratch("compat-missing");
        let ghost = member("ghost", ActivationTiming::AfterStart, vec![(dir.join("absent.json"), Usage::Attach)]);
        assert_eq!(codes(&check(&[ghost], Some(CONFIG)), Severity::Error), vec!["pdu_file_unreadable"]);
    }

    /// 書き手のいない読みと、複数の書き手は Warning。compact の所有者は書き手の可能性として数える。
    #[test]
    fn writer_warnings() {
        let dir = scratch("compat-writers");
        let owner_legacy = member("owner", ActivationTiming::BeforeStart, vec![(legacy(&dir, "owner.json", "Rig", &[(0, "cmd", 136)], &[]), Usage::Register)]);
        let reader = member("reader", ActivationTiming::AfterStart, vec![(legacy(&dir, "reader.json", "Rig", &[(0, "cmd", 136)], &[]), Usage::Attach)]);
        assert!(codes(&check(&[owner_legacy, reader.clone()], Some(CONFIG)), Severity::Warning).contains(&"no_writer".to_owned()));

        let owner_compact = member("owner", ActivationTiming::BeforeStart, vec![(compact_owner(&dir, "Rig", &[(0, "cmd", 136)]), Usage::Register)]);
        assert!(!codes(&check(&[owner_compact.clone(), reader], Some(CONFIG)), Severity::Warning).contains(&"no_writer".to_owned()));

        let w1 = member("w1", ActivationTiming::AfterStart, vec![(legacy(&dir, "w1.json", "Rig", &[], &[(0, "cmd", 136)]), Usage::Attach)]);
        let w2 = member("w2", ActivationTiming::AfterStart, vec![(legacy(&dir, "w2.json", "Rig", &[], &[(0, "cmd", 136)]), Usage::Attach)]);
        assert_eq!(codes(&check(&[owner_compact, w1, w2], Some(CONFIG)), Severity::Warning), vec!["multiple_writers"]);
    }

    /// 同じ robot を 2 つのメンバーが登録すると Warning（矛盾がなければ Error ではない）。
    #[test]
    fn two_registrants_of_one_robot_is_a_warning() {
        let dir = scratch("compat-shared");
        let a = member("a", ActivationTiming::BeforeStart, vec![(compact_owner(&dir, "Rig", &[(0, "cmd", 136)]), Usage::Register)]);
        let b = member("b", ActivationTiming::BeforeStart, vec![(legacy(&dir, "b.json", "Rig", &[], &[(1, "attitude", 72)]), Usage::Register)]);
        let report = check(&[a, b], Some(CONFIG));
        assert!(codes(&report, Severity::Error).is_empty(), "{:#?}", report.findings);
        assert!(codes(&report, Severity::Warning).contains(&"shared_robot_registration".to_owned()));
    }

    /// gimbal-drone-demo の実設定で、既定の構成（viz-mujoco を除く 5 つ）は Error にならない。
    ///
    /// 設定のある場所を `HDM_GIMBAL_CONFIG_DIR` で渡したときだけ実行する。
    #[test]
    fn gimbal_drone_demo_configs_are_compatible() {
        let Some(dir) = std::env::var_os("HDM_GIMBAL_CONFIG_DIR").map(PathBuf::from) else { return };
        let f = |name: &str| dir.join(name);
        let members = vec![
            member("rig-owner", ActivationTiming::BeforeStart, vec![(f("custom.json"), Usage::Register)]),
            member("drone-mock", ActivationTiming::BeforeStart, vec![(f("drone_mock_custom.json"), Usage::Register)]),
            member("rig-sim", ActivationTiming::AfterStart, vec![(f("service_rig.json"), Usage::Attach), (f("pdu_config_rig.json"), Usage::Attach)]),
            member("viz-foxglove", ActivationTiming::AfterStart, vec![(f("service_viewer_full.json"), Usage::Attach), (f("pdu_config_viewer_full.json"), Usage::Attach)]),
            member("pilot", ActivationTiming::AfterStart, vec![(f("service_pilot_full.json"), Usage::Attach), (f("pdu_config_pilot_full.json"), Usage::Attach)]),
        ];
        let owned: usize = members.iter().map(|m| m.ports.registrations.len()).sum();
        let report = check(&members, Some(CONFIG));
        assert_eq!(owned, 27);
        assert!(!report.has_errors(), "{:#?}", report.findings);
        assert_eq!(report.coverage, Coverage::Complete);
        for finding in &report.findings {
            println!("{:?} {} {}", finding.severity, finding.code, finding.message);
        }

        // viz-mujoco も足しても Error にならない。
        let mut with_mujoco = members;
        with_mujoco.push(member("viz-mujoco", ActivationTiming::AfterStart, vec![(f("pdu_config_viewer_full.json"), Usage::Attach)]));
        assert!(!check(&with_mujoco, Some(CONFIG)).has_errors());
    }
}
