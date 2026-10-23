//! プロジェクトの接続チェックと、利用者の承認との照合。
//!
//! メンバーごとに、PDU 設定の入口からポートを抽出し（`ports`）、プロジェクト全体で
//! 突き合わせる（`compat`）。あわせて、起動定義と PDU 設定の内容から revision を求め、
//! 利用者が承認したときの revision と比べる。一括起動は、接続チェックの Error か、
//! 承認後に変わったメンバーがあれば止める。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::compat::{self, CompatReport, MemberPorts};
use crate::model::{Acceptance, ChannelUse, Library, LibraryAsset, Member, PduEntrySpec, Project, Variant, CORE_NAMESPACE_VARIABLE};
use crate::ports::{self, FileDigest, PduEntry, PortSet, Usage};
use crate::types::{ActivationTiming, ProgramSpec, ReadinessCheck};

/// 承認との照合の結果。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceState {
    /// PDU 設定がない（接続チェックの対象外）。
    NoPduConfig,
    /// 承認した時点から変わっていない。
    Accepted,
    /// 承認した後に、起動定義か PDU 設定が変わった。
    Changed,
    /// まだ一度も承認していない。
    NotAccepted,
    /// この PC からは確かめられない（WSL で動くアセットなど）。一括起動は止めない。
    Unverified,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberCheck {
    pub member_id: String,
    pub name: String,
    /// 一括起動の対象か。対象外のメンバーは、接続チェックにも一括起動の判定にも入れない。
    pub enabled: bool,
    /// 使った PDU 設定の入口。
    pub entries: Vec<PduEntrySpec>,
    /// 入口を `config_files` から推定したか。
    pub inferred: bool,
    pub revision: String,
    pub manifest: Vec<FileDigest>,
    pub acceptance: AcceptanceState,
    /// 承認した時点からの変化（ファイルの追加・削除・変更、起動定義の変更）。
    pub changes: Vec<String>,
    /// 承認の対象になる起動の内容。承認する画面に、PDU の宣言と一緒に見せる。
    pub contract: LaunchContract,
    /// PDU 設定のファイルをこの PC から読んで確かめたか（WSL で動くメンバーは読めない）。
    /// 承認の状態とは別に持つ。
    pub files_checked: bool,
    /// 今の PDU 設定から読んだチャネル。
    pub channels: Vec<ChannelUse>,
    /// 承認した時点のチャネルからの変化。変化がなければ空。
    pub port_changes: Vec<String>,
}

/// メンバーの起動のしかた。環境変数はアセットの値にメンバーの上書きを重ねたもの。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchContract {
    pub launch: ProgramSpec,
    pub activation_timing: ActivationTiming,
    pub readiness: ReadinessCheck,
    pub depends_on: Vec<String>,
    pub optional_depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCheck {
    /// どのプロジェクトの結果か。画面は、表示中のプロジェクトと一致するときだけ使う。
    pub project_id: String,
    pub report: CompatReport,
    pub members: Vec<MemberCheck>,
}

impl ProjectCheck {
    /// 一括起動を止める理由。空なら起動してよい。
    pub fn blocking_reasons(&self) -> Vec<String> {
        let mut reasons: Vec<String> = self.report.findings.iter().filter(|finding| finding.severity == compat::Severity::Error).map(|finding| finding.message.clone()).collect();
        for member in self.members.iter().filter(|member| member.enabled) {
            match member.acceptance {
                AcceptanceState::Changed => reasons.push(format!("{} の起動定義か PDU 設定が、承認した後に変わっています（{}）。接続チェックで確認して承認してください。", member.name, member.changes.join("、"))),
                AcceptanceState::NotAccepted => reasons.push(format!("{} の起動定義と PDU 設定はまだ承認されていません。接続チェックで確認して承認してください。", member.name)),
                AcceptanceState::NoPduConfig | AcceptanceState::Accepted | AcceptanceState::Unverified => {}
            }
        }
        reasons
    }
}

/// 入口のパスを、アセットの作業ディレクトリ基準で解決する。
fn resolve_entry_path(path: &str, variant: &Variant) -> PathBuf {
    let path = Path::new(path);
    match &variant.launch.cwd {
        Some(cwd) if path.is_relative() => Path::new(cwd).join(path),
        _ => path.to_path_buf(),
    }
}

/// メンバーが使う PDU 設定の入口。バリエーションに登録されていればそれを、なければ
/// `config_files` から推定する（推定は保存しない）。
///
/// 推定の規則: `paths` を持つ compact 形式は、Core の開始前に起動するなら登録
/// （`asset_register` に渡す）、それ以外は接続とみなす。`robots` を持つ legacy 形式は
/// 接続。service 設定は、変更の検知とカバレッジのために接続として入れる。pdutypes の
/// ような配列や、PDU に関係しない設定は入れない。
///
/// 読めない、または JSON として壊れている `.json` は、入口として残す。黙って外すと、
/// 承認した後に PDU 設定が消えたり壊れたりしたメンバーが、確認の対象から抜けてしまう。
/// 抽出の段階で問題として報告され、一括起動を止める。
pub fn effective_entries(variant: &Variant, timing: &ActivationTiming) -> (Vec<PduEntrySpec>, bool) {
    if !variant.pdu_entries.is_empty() {
        return (variant.pdu_entries.clone(), false);
    }
    let mut entries = Vec::new();
    for file in &variant.config_files {
        let is_json = Path::new(file).extension().is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
        let parsed = std::fs::read(resolve_entry_path(file, variant)).ok().and_then(|bytes| serde_json::from_str::<Value>(String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}')).ok());
        let Some(value) = parsed else {
            if is_json {
                entries.push(PduEntrySpec { path: file.clone(), usage: Usage::Attach, usage_inferred: true });
            }
            continue;
        };
        let usage = if value.get("paths").is_some() {
            if *timing == ActivationTiming::BeforeStart { Usage::Register } else { Usage::Attach }
        } else if value.get("robots").is_some() || value.get("pdu_config_path").is_some() || value.get("services").is_some() {
            Usage::Attach
        } else {
            continue;
        };
        entries.push(PduEntrySpec { path: file.clone(), usage, usage_inferred: true });
    }
    (entries, true)
}

/// 入口からポートを抽出し、手動の向き指定を当てる。
pub fn member_ports(variant: &Variant, entries: &[PduEntrySpec]) -> PortSet {
    let resolved: Vec<PduEntry> = entries.iter().map(|entry| PduEntry { path: resolve_entry_path(&entry.path, variant), usage: entry.usage }).collect();
    let mut ports = ports::extract(&resolved);
    for manual in &variant.manual_access {
        let mut matched = false;
        for access in ports.access.iter_mut().filter(|access| access.channel.robot == manual.robot && access.channel.channel_id == manual.channel_id) {
            access.access = manual.access;
            matched = true;
        }
        if !matched {
            ports.problems.push(format!("手動の向き指定（{} ch{}）に対応するチャネルが、PDU 設定にありません", manual.robot, manual.channel_id));
        }
    }
    ports
}

fn extra_digests(variant: &Variant) -> Vec<FileDigest> {
    variant
        .extra_inputs
        .iter()
        .map(|file| {
            let path = resolve_entry_path(file, variant);
            let sha256 = std::fs::read(&path).map(|bytes| hex::encode(Sha256::digest(&bytes))).unwrap_or_else(|_| "missing".to_owned());
            FileDigest { path: path.display().to_string(), sha256 }
        })
        .collect()
}

/// 起動定義・入口・手動の向き指定・環境の上書き・読んだファイルの内容から求める revision。
fn revision(variant: &Variant, member: &Member, entries: &[PduEntrySpec], manifest: &[FileDigest], problems: &[String]) -> String {
    let material = json!({
        "launch": variant.launch,
        // 起動の段取り（タイミング・準備完了の条件・依存）も、起動のしかたの一部として含める。
        "startup": {
            "activation_timing": member.activation_timing.clone().unwrap_or_else(|| variant.activation_timing.clone()),
            "readiness": member.readiness.clone().unwrap_or_else(|| variant.readiness.clone()),
            "depends_on": member.depends_on,
            "optional_depends_on": member.optional_depends_on,
        },
        "entries": entries,
        "manual_access": variant.manual_access,
        "extra_inputs": variant.extra_inputs,
        "env_overrides": member.env_overrides,
        "manifest": manifest,
        // 読めないファイルは manifest に載らないので、問題の内容も含めて変化を捉える。
        "problems": problems,
    });
    hex::encode(Sha256::digest(serde_json::to_vec(&material).unwrap_or_default()))
}

fn describe_changes(accepted: &Acceptance, manifest: &[FileDigest]) -> Vec<String> {
    let before: BTreeMap<&str, &str> = accepted.manifest.iter().map(|digest| (digest.path.as_str(), digest.sha256.as_str())).collect();
    let after: BTreeMap<&str, &str> = manifest.iter().map(|digest| (digest.path.as_str(), digest.sha256.as_str())).collect();
    let name = |path: &str| Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_owned());
    let mut changes = Vec::new();
    for (path, hash) in &after {
        match before.get(path) {
            None => changes.push(format!("{} が増えた", name(path))),
            Some(old) if old != hash => changes.push(format!("{} が変わった", name(path))),
            _ => {}
        }
    }
    for path in before.keys().filter(|path| !after.contains_key(*path)) {
        changes.push(format!("{} がなくなった", name(path)));
    }
    if changes.is_empty() {
        changes.push("起動定義か起動の段取りが変わった".to_owned());
    }
    changes
}

/// ポートを、宣言ごとの使い方にまとめる。同じチャネルでも名前・型・サイズの違う宣言は
/// 別の要素にする（矛盾を 1 つの代表に丸めて隠さないため）。
pub fn channel_uses(ports: &PortSet) -> Vec<ChannelUse> {
    type Key = (String, i64, String, String, i64);
    fn slot<'a>(uses: &'a mut BTreeMap<Key, ChannelUse>, decl: &ports::ChannelDecl) -> &'a mut ChannelUse {
        let key = (decl.robot.clone(), decl.channel_id, decl.org_name.clone(), decl.type_name.clone(), decl.pdu_size);
        uses.entry(key).or_insert_with(|| ChannelUse { robot: decl.robot.clone(), channel_id: decl.channel_id, org_name: decl.org_name.clone(), type_name: decl.type_name.clone(), pdu_size: decl.pdu_size, registers: false, access: Vec::new() })
    }
    let mut uses = BTreeMap::new();
    for decl in &ports.registrations {
        slot(&mut uses, decl).registers = true;
    }
    for access in &ports.access {
        let channel = slot(&mut uses, &access.channel);
        if !channel.access.contains(&access.access) {
            channel.access.push(access.access);
            channel.access.sort();
        }
    }
    uses.into_values().collect()
}

/// 承認した時点のチャネルと今のチャネルの違いを、文で返す。
fn describe_port_changes(accepted: &Acceptance, current: &[ChannelUse]) -> Vec<String> {
    if accepted.channels.is_empty() {
        return if accepted.had_pdu_entries && !current.is_empty() { vec!["以前の承認にはチャネルの記録がありません".to_owned()] } else { Vec::new() };
    }
    fn group(channels: &[ChannelUse]) -> BTreeMap<(String, i64), Vec<&ChannelUse>> {
        let mut groups: BTreeMap<(String, i64), Vec<&ChannelUse>> = BTreeMap::new();
        for channel in channels {
            groups.entry((channel.robot.clone(), channel.channel_id)).or_default().push(channel);
        }
        groups
    }
    let describe = |uses: &[&ChannelUse]| uses.iter().map(|use_| format!("{} {} / {}", use_.org_name, use_.type_name, use_.pdu_size)).collect::<Vec<_>>().join("、");
    let before = group(&accepted.channels);
    let after = group(current);
    let mut changes = Vec::new();
    for (key, now) in &after {
        let label = format!("{} ch{}", key.0, key.1);
        match before.get(key) {
            None => changes.push(format!("{label}（{}）が増えた", describe(now))),
            Some(old) if old != now => match (old.as_slice(), now.as_slice()) {
                ([old], [new]) if old.org_name == new.org_name => {
                    let mut parts = Vec::new();
                    if old.type_name != new.type_name {
                        parts.push(format!("型 {} → {}", old.type_name, new.type_name));
                    }
                    if old.pdu_size != new.pdu_size {
                        parts.push(format!("サイズ {} → {}", old.pdu_size, new.pdu_size));
                    }
                    if old.registers != new.registers {
                        parts.push(if new.registers { "登録するようになった".to_owned() } else { "登録しなくなった".to_owned() });
                    }
                    if old.access != new.access {
                        parts.push("向きが変わった".to_owned());
                    }
                    changes.push(format!("{label}（{}）: {}", new.org_name, parts.join("、")));
                }
                _ => changes.push(format!("{label}: 宣言が変わった（{} → {}）", describe(old), describe(now))),
            },
            _ => {}
        }
    }
    for (key, old) in before.iter().filter(|(key, _)| !after.contains_key(*key)) {
        changes.push(format!("{} ch{}（{}）がなくなった", key.0, key.1, describe(old)));
    }
    changes
}

struct Evaluated {
    check: MemberCheck,
    ports: MemberPorts,
}

fn evaluate(member: &Member, asset: &LibraryAsset) -> Result<Evaluated, String> {
    let variant = asset.variant(&member.variant_id).ok_or_else(|| format!("メンバー「{}」のバリエーションがありません。", member.id))?;
    let timing = member.activation_timing.clone().unwrap_or_else(|| variant.activation_timing.clone());
    // WSL で動くアセットのファイルは、この PC のファイルとしては読めない。読んだつもりで
    // 誤って通したり止めたりしないよう、確かめずにその旨を示す。
    let wsl = matches!(variant.launch.target, crate::types::ExecutionTarget::Wsl { .. });
    let (entries, inferred) = if wsl { (Vec::new(), false) } else { effective_entries(variant, &timing) };
    let ports = member_ports(variant, &entries);
    let mut manifest = ports.files.clone();
    if !wsl {
        manifest.extend(extra_digests(variant));
    }
    let revision = revision(variant, member, &entries, &manifest, &ports.problems);
    // 承認は、PDU 設定の有無や実行先にかかわらず、起動の内容（revision）ごとに求める。
    // 承認したままの内容なら、確かめた範囲に応じて状態を分ける。
    let (acceptance, changes) = match &member.accepted {
        None => (AcceptanceState::NotAccepted, Vec::new()),
        Some(accepted) if accepted.revision != revision => (AcceptanceState::Changed, describe_changes(accepted, &manifest)),
        Some(_) if wsl => (AcceptanceState::Unverified, Vec::new()),
        Some(_) if entries.is_empty() => (AcceptanceState::NoPduConfig, Vec::new()),
        Some(_) => (AcceptanceState::Accepted, Vec::new()),
    };
    let channels = channel_uses(&ports);
    let port_changes = match (&member.accepted, acceptance) {
        (Some(accepted), AcceptanceState::Changed) => describe_port_changes(accepted, &channels),
        _ => Vec::new(),
    };
    let name = member.name.clone().unwrap_or_else(|| asset.name.clone());
    let mut env = variant.launch.env.clone();
    env.extend(member.env_overrides.clone());
    let contract = LaunchContract {
        launch: ProgramSpec { env: env.clone(), ..variant.launch.clone() },
        activation_timing: timing.clone(),
        readiness: member.readiness.clone().unwrap_or_else(|| variant.readiness.clone()),
        depends_on: member.depends_on.clone(),
        optional_depends_on: member.optional_depends_on.clone(),
    };
    Ok(Evaluated {
        ports: MemberPorts { member_id: member.id.clone(), name: name.clone(), activation_timing: timing, hako_config_path: env.get(CORE_NAMESPACE_VARIABLE).cloned(), ports },
        check: MemberCheck { member_id: member.id.clone(), name, enabled: member.enabled, entries, inferred, revision, manifest, acceptance, changes, contract, files_checked: !wsl, channels, port_changes },
    })
}

/// プロジェクトを検査する。接続チェックには、一括起動の対象のメンバーだけを入れる。
pub fn check_project(project: &Project, library: &Library) -> Result<ProjectCheck, String> {
    project.validate(library)?;
    let mut members = Vec::new();
    let mut ports = Vec::new();
    for member in &project.members {
        let evaluated = evaluate(member, &library[&member.asset_id])?;
        if member.enabled && !evaluated.check.entries.is_empty() {
            ports.push(evaluated.ports);
        }
        members.push(evaluated.check);
    }
    let mut report = compat::check(&ports, project.core_env.get(CORE_NAMESPACE_VARIABLE).map(String::as_str));
    // 確かめられないメンバーがいれば、そのメンバーが登録するチャネルかもしれないので
    // 「登録者がいない」は止めずに注意にとどめる。
    let unverified: Vec<&str> = members.iter().filter(|member| member.enabled && !member.files_checked).map(|member| member.name.as_str()).collect();
    if !unverified.is_empty() {
        report.coverage = crate::ports::Coverage::Partial;
        for finding in report.findings.iter_mut().filter(|finding| finding.code == "no_owner") {
            finding.severity = compat::Severity::Warning;
            finding.message = format!("{}（確かめられないメンバー {} が登録している可能性があります）", finding.message, unverified.join("、"));
        }
        report.findings.push(compat::Finding { severity: compat::Severity::Info, code: "unverified_member".to_owned(), members: Vec::new(), robot: None, channel_id: None, message: format!("この PC からは確かめられないメンバー: {}", unverified.join("、")) });
        report.findings.sort_by(|a, b| a.severity.cmp(&b.severity).then_with(|| a.code.cmp(&b.code)));
    }
    Ok(ProjectCheck { project_id: project.id.clone(), report, members })
}

/// `member_ids` のメンバーを、今の内容で承認する。
pub fn accept_members(project: &mut Project, library: &Library, member_ids: &[String]) -> Result<(), String> {
    accept_revisions(project, library, member_ids.iter().map(|id| (id.clone(), None)).collect())
}

/// 画面が表示していた revision を添えて承認する。どれか 1 つでも今の内容と違えば、
/// 何も承認しない。表示した後にファイルが変わっていれば、利用者はその変化を見ていない。
pub fn accept_shown(project: &mut Project, library: &Library, shown: &BTreeMap<String, String>) -> Result<(), String> {
    accept_revisions(project, library, shown.iter().map(|(id, revision)| (id.clone(), Some(revision.clone()))).collect())
}

fn accept_revisions(project: &mut Project, library: &Library, wanted: Vec<(String, Option<String>)>) -> Result<(), String> {
    let mut accepted = Vec::new();
    for (id, shown) in wanted {
        let member = project.members.iter().find(|member| member.id == id).ok_or_else(|| format!("メンバー「{id}」がありません。"))?;
        let asset = library.get(&member.asset_id).ok_or_else(|| format!("メンバー「{id}」のアセットがライブラリにありません。"))?;
        let evaluated = evaluate(member, asset)?;
        if shown.as_ref().is_some_and(|shown| shown != &evaluated.check.revision) {
            return Err(format!("{} の内容が、表示した後に変わっています。接続チェックを見直してから承認してください。", evaluated.check.name));
        }
        let had_pdu_entries = !evaluated.check.entries.is_empty();
        accepted.push((id, Acceptance { revision: evaluated.check.revision, manifest: evaluated.check.manifest, accepted_at: Utc::now(), had_pdu_entries, channels: evaluated.check.channels }));
    }
    for (id, acceptance) in accepted {
        if let Some(member) = project.members.iter_mut().find(|member| member.id == id) {
            member.accepted = Some(acceptance);
        }
    }
    Ok(())
}

/// すべてのメンバーを承認する。取り込みや移行のように、利用者が構成を明示的に入れた
/// ときに使う。
pub fn accept_all(project: &mut Project, library: &Library) -> Result<(), String> {
    let ids: Vec<String> = project.members.iter().map(|member| member.id.clone()).collect();
    accept_members(project, library, &ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{library_asset_from_definition, member_from_definition};
    use crate::ports::tests::{compact_owner, legacy, scratch};
    use crate::ports::Access;
    use crate::types::{AssetDefinition, AssetRole, ProgramSpec, ReadinessCheck};

    const CONFIG: &str = "C:/hako/cpp_core_config.json";

    fn definition(id: &str, timing: ActivationTiming, cwd: &Path, config_files: Vec<&str>) -> AssetDefinition {
        AssetDefinition {
            id: id.to_owned(),
            name: id.to_owned(),
            role: AssetRole::Other,
            command: ProgramSpec { program: "python".to_owned(), args: vec![format!("{id}.py")], cwd: Some(cwd.display().to_string()), env: BTreeMap::from([(CORE_NAMESPACE_VARIABLE.to_owned(), CONFIG.to_owned())]), target: Default::default() },
            depends_on: Vec::new(),
            optional_depends_on: Vec::new(),
            activation_timing: timing,
            config_files: config_files.into_iter().map(str::to_owned).collect(),
            enabled: true,
            readiness: ReadinessCheck::Manual,
        }
    }

    /// owner（compact、登録）と sim（legacy、接続）の 2 メンバーのプロジェクト。設定は相対パスで、
    /// アセットの作業ディレクトリ基準で解決される。
    fn project(name: &str) -> (PathBuf, Project, Library) {
        let dir = scratch(name);
        compact_owner(&dir, "Rig", &[(0, "cmd", 136), (1, "attitude", 72)]);
        legacy(&dir, "sim.json", "Rig", &[(0, "cmd", 136)], &[(1, "attitude", 72)]);
        std::fs::write(dir.join("other.json"), br#"{ "max_angle_rad": 0.35 }"#).unwrap();
        let owner = definition("owner", ActivationTiming::BeforeStart, &dir, vec!["Rig-custom.json", "Rig-pdutypes.json"]);
        let sim = definition("sim", ActivationTiming::AfterStart, &dir, vec!["sim.json", "other.json"]);
        let mut library = Library::new();
        let mut members = Vec::new();
        for definition in [&owner, &sim] {
            let asset = library_asset_from_definition(format!("asset-{}", definition.id), definition);
            members.push(member_from_definition(definition, asset.id.clone()));
            library.insert(asset.id.clone(), asset);
        }
        let mut project = Project::empty("p".to_owned(), "P".to_owned());
        project.core_env.insert(CORE_NAMESPACE_VARIABLE.to_owned(), CONFIG.to_owned());
        project.members = members;
        (dir, project, library)
    }

    /// config_files から入口を推定する。compact は開始前なら登録、legacy は接続、PDU 以外と pdutypes は入れない。
    #[test]
    fn entries_are_inferred_from_config_files() {
        let (_, project, library) = project("check-infer");
        let check = check_project(&project, &library).unwrap();
        let owner = &check.members[0];
        assert!(owner.inferred);
        assert_eq!(owner.entries.iter().map(|e| (e.path.as_str(), e.usage)).collect::<Vec<_>>(), vec![("Rig-custom.json", Usage::Register)]);
        let sim = &check.members[1];
        assert_eq!(sim.entries.iter().map(|e| (e.path.as_str(), e.usage)).collect::<Vec<_>>(), vec![("sim.json", Usage::Attach)]);
        assert!(!check.report.has_errors(), "{:#?}", check.report.findings);
    }

    /// 承認していなければ一括起動を止め、承認すれば通す。承認後に設定が変われば、また止める。
    #[test]
    fn acceptance_gates_start_all_until_changes_are_accepted() {
        let (dir, mut project, library) = project("check-accept");
        assert_eq!(check_project(&project, &library).unwrap().blocking_reasons().len(), 2, "まだ承認していない 2 メンバー");
        accept_all(&mut project, &library).unwrap();
        let check = check_project(&project, &library).unwrap();
        assert!(check.blocking_reasons().is_empty(), "{:?}", check.blocking_reasons());
        assert!(check.members.iter().all(|m| m.acceptance == AcceptanceState::Accepted));

        // sim の設定を書き換える（サイズは変えず、名前の表記だけ変える）。
        legacy(&dir, "sim.json", "Rig", &[(0, "cmd", 136)], &[(1, "attitude", 72), (1, "attitude", 72)]);
        let check = check_project(&project, &library).unwrap();
        let sim = check.members.iter().find(|m| m.member_id == "sim").unwrap();
        assert_eq!(sim.acceptance, AcceptanceState::Changed);
        assert_eq!(sim.changes, vec!["sim.json が変わった"]);
        assert_eq!(check.blocking_reasons().len(), 1);
        accept_members(&mut project, &library, &["sim".to_owned()]).unwrap();
        assert!(check_project(&project, &library).unwrap().blocking_reasons().is_empty());
    }

    /// 承認した時点のチャネルを記録し、変わったチャネルを差分として示す。
    #[test]
    fn port_changes_name_the_changed_channels() {
        let (dir, mut project, library) = project("check-port-changes");
        accept_all(&mut project, &library).unwrap();
        let sim = project.member("sim").unwrap().accepted.clone().unwrap();
        assert_eq!(sim.channels.iter().map(|c| (c.channel_id, c.registers, c.access.clone())).collect::<Vec<_>>(), vec![(0, false, vec![Access::Read]), (1, false, vec![Access::Write])]);
        legacy(&dir, "sim.json", "Rig", &[(0, "cmd", 144), (2, "imu", 72)], &[]);
        let check = check_project(&project, &library).unwrap();
        let changes = &check.members.iter().find(|m| m.member_id == "sim").unwrap().port_changes;
        assert!(changes.iter().any(|c| c.contains("ch0") && c.contains("サイズ 136 → 144")), "{changes:?}");
        assert!(changes.iter().any(|c| c.contains("ch2") && c.contains("増えた")), "{changes:?}");
        assert!(changes.iter().any(|c| c.contains("ch1") && c.contains("なくなった")), "{changes:?}");
        // 承認済みのメンバーには差分を出さない。
        assert!(check.members.iter().find(|m| m.member_id == "owner").unwrap().port_changes.is_empty());
    }

    /// 起動定義（引数）を変えても、ファイルが同じなら「起動定義が変わった」として止める。
    #[test]
    fn launch_changes_also_need_acceptance() {
        let (_, mut project, mut library) = project("check-launch");
        accept_all(&mut project, &library).unwrap();
        library.get_mut("asset-sim").unwrap().variants[0].launch.args.push("--legs".to_owned());
        let check = check_project(&project, &library).unwrap();
        let sim = check.members.iter().find(|m| m.member_id == "sim").unwrap();
        assert_eq!(sim.acceptance, AcceptanceState::Changed);
        assert_eq!(sim.changes, vec!["起動定義か起動の段取りが変わった"]);
    }

    /// 起動タイミングや依存を変えても、承認が要る。
    #[test]
    fn startup_changes_need_acceptance() {
        let (_, mut project, library) = project("check-startup");
        accept_all(&mut project, &library).unwrap();
        project.members[1].activation_timing = Some(ActivationTiming::Manual);
        assert_eq!(check_project(&project, &library).unwrap().members[1].acceptance, AcceptanceState::Changed);
        accept_all(&mut project, &library).unwrap();
        project.members[1].depends_on.push("owner".to_owned());
        assert_eq!(check_project(&project, &library).unwrap().members[1].acceptance, AcceptanceState::Changed);
    }

    /// 承認した後に PDU 設定が消えたり壊れたりすると、確認から抜けずに止める。
    #[test]
    fn deleted_or_broken_configs_after_acceptance_block() {
        let (dir, mut project, library) = project("check-deleted");
        accept_all(&mut project, &library).unwrap();
        std::fs::write(dir.join("sim.json"), b"{ truncated").unwrap();
        let check = check_project(&project, &library).unwrap();
        assert!(check.report.has_errors(), "{:#?}", check.report.findings);
        assert!(!check.blocking_reasons().is_empty());
        std::fs::remove_file(dir.join("sim.json")).unwrap();
        let check = check_project(&project, &library).unwrap();
        assert!(!check.blocking_reasons().is_empty());
        assert_ne!(check.members[1].acceptance, AcceptanceState::NoPduConfig);
    }

    /// 承認した後に config_files から PDU 設定を外すと、変化として止める。
    #[test]
    fn removing_the_pdu_config_after_acceptance_is_a_change() {
        let (_, mut project, mut library) = project("check-removed");
        accept_all(&mut project, &library).unwrap();
        library.get_mut("asset-sim").unwrap().variants[0].config_files = vec!["other.json".to_owned()];
        let check = check_project(&project, &library).unwrap();
        assert_eq!(check.members[1].acceptance, AcceptanceState::Changed);
        assert!(check.members[1].changes.iter().any(|change| change.contains("sim.json")), "{:?}", check.members[1].changes);
    }

    /// 表示した revision と今の内容が違えば、何も承認しない。
    #[test]
    fn acceptance_rejects_content_changed_after_it_was_shown() {
        let (dir, mut project, library) = project("check-shown");
        let shown: BTreeMap<String, String> = check_project(&project, &library).unwrap().members.iter().map(|m| (m.member_id.clone(), m.revision.clone())).collect();
        legacy(&dir, "sim.json", "Rig", &[(0, "cmd", 136)], &[(1, "attitude", 72), (1, "attitude", 72)]);
        assert!(accept_shown(&mut project, &library, &shown).is_err());
        assert!(project.members.iter().all(|m| m.accepted.is_none()), "一部だけ承認しない");
        let shown: BTreeMap<String, String> = check_project(&project, &library).unwrap().members.iter().map(|m| (m.member_id.clone(), m.revision.clone())).collect();
        accept_shown(&mut project, &library, &shown).unwrap();
        assert!(check_project(&project, &library).unwrap().blocking_reasons().is_empty());
    }

    /// PDU 設定がなく、監視するファイル（extra_inputs）だけを持つメンバーは、承認しても
    /// 「変更あり」のまま残らない。
    #[test]
    fn extra_inputs_without_pdu_entries_do_not_stay_changed() {
        let (dir, mut project, mut library) = project("check-extra");
        std::fs::write(dir.join("script.py"), b"PDU_SIZE = 136").unwrap();
        let sim = &mut library.get_mut("asset-sim").unwrap().variants[0];
        sim.config_files = vec!["other.json".to_owned()];
        sim.extra_inputs = vec!["script.py".to_owned()];
        accept_all(&mut project, &library).unwrap();
        let check = check_project(&project, &library).unwrap();
        assert_eq!(check.members[1].acceptance, AcceptanceState::NoPduConfig);
        assert!(check.blocking_reasons().is_empty(), "{:?}", check.blocking_reasons());
    }

    /// 読めない入口を承認した後に、その入口を外しても、変化として止める。
    #[test]
    fn removing_an_unreadable_entry_after_acceptance_is_a_change() {
        let (dir, mut project, mut library) = project("check-unreadable-removed");
        std::fs::remove_file(dir.join("sim.json")).unwrap();
        accept_all(&mut project, &library).unwrap();
        library.get_mut("asset-sim").unwrap().variants[0].config_files = vec!["other.json".to_owned()];
        assert_eq!(check_project(&project, &library).unwrap().members[1].acceptance, AcceptanceState::Changed);
    }

    /// WSL で動くメンバーは確かめず、一括起動は止めない。そのメンバーが登録しうるチャネルの
    /// 「登録者がいない」は注意にとどめる。
    #[test]
    fn wsl_members_are_reported_as_unverified() {
        let (_, mut project, mut library) = project("check-wsl");
        library.get_mut("asset-owner").unwrap().variants[0].launch.target = crate::types::ExecutionTarget::Wsl { distribution: "Ubuntu".to_owned() };
        accept_all(&mut project, &library).unwrap();
        let check = check_project(&project, &library).unwrap();
        assert_eq!(check.members[0].acceptance, AcceptanceState::Unverified);
        assert!(check.blocking_reasons().is_empty(), "{:?}", check.blocking_reasons());
        assert!(check.report.findings.iter().any(|f| f.code == "no_owner" && f.severity == compat::Severity::Warning));
        assert_eq!(check.report.coverage, crate::ports::Coverage::Partial);
    }

    /// WSL のメンバーでも、承認していないか、承認した後に起動定義が変われば止める。
    #[test]
    fn wsl_members_still_need_acceptance_of_their_launch() {
        let (_, mut project, mut library) = project("check-wsl-changed");
        library.get_mut("asset-owner").unwrap().variants[0].launch.target = crate::types::ExecutionTarget::Wsl { distribution: "Ubuntu".to_owned() };
        assert_eq!(check_project(&project, &library).unwrap().members[0].acceptance, AcceptanceState::NotAccepted);
        accept_all(&mut project, &library).unwrap();
        library.get_mut("asset-owner").unwrap().variants[0].launch.args.push("--other".to_owned());
        let check = check_project(&project, &library).unwrap();
        assert_eq!(check.members[0].acceptance, AcceptanceState::Changed);
        assert_eq!(check.blocking_reasons().len(), 1, "{:?}", check.blocking_reasons());
    }

    /// 承認しても、接続チェックの Error は一括起動を止める。
    #[test]
    fn accepted_members_with_incompatible_ports_still_block() {
        let (dir, mut project, library) = project("check-error");
        legacy(&dir, "sim.json", "Rig", &[(0, "cmd", 144)], &[]);
        accept_all(&mut project, &library).unwrap();
        let reasons = check_project(&project, &library).unwrap().blocking_reasons();
        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].contains("144"), "{reasons:?}");
    }

    /// 一括起動の対象外のメンバーは、接続チェックにも一括起動の判定にも入れない。
    #[test]
    fn excluded_members_are_left_out() {
        let (dir, mut project, library) = project("check-excluded");
        legacy(&dir, "sim.json", "Rig", &[(0, "cmd", 144)], &[]);
        project.members[1].enabled = false;
        accept_members(&mut project, &library, &["owner".to_owned()]).unwrap();
        assert!(check_project(&project, &library).unwrap().blocking_reasons().is_empty());
    }

    /// PDU 設定のないメンバーは対象外で、承認も求めない。
    #[test]
    fn members_without_pdu_config_still_need_their_launch_accepted() {
        let (_, mut project, mut library) = project("check-none");
        library.get_mut("asset-sim").unwrap().variants[0].config_files = vec!["other.json".to_owned()];
        accept_members(&mut project, &library, &["owner".to_owned()]).unwrap();
        let check = check_project(&project, &library).unwrap();
        assert_eq!(check.members[1].acceptance, AcceptanceState::NotAccepted);
        assert_eq!(check.blocking_reasons().len(), 1);
        accept_members(&mut project, &library, &["sim".to_owned()]).unwrap();
        let check = check_project(&project, &library).unwrap();
        assert_eq!(check.members[1].acceptance, AcceptanceState::NoPduConfig);
        assert!(check.blocking_reasons().is_empty());
        // 起動の内容が変われば、PDU 設定がなくても承認を求める。
        library.get_mut("asset-sim").unwrap().variants[0].launch.args.push("--other".to_owned());
        assert_eq!(check_project(&project, &library).unwrap().members[1].acceptance, AcceptanceState::Changed);
    }

    /// WSL で承認したメンバーを、PDU 設定のないローカルのバリエーションに替えても承認を求める。
    #[test]
    fn switching_from_wsl_to_native_needs_acceptance() {
        let (_, mut project, mut library) = project("check-wsl-native");
        let sim = library.get_mut("asset-sim").unwrap();
        let mut native = sim.variants[0].clone();
        native.id = "native".to_owned();
        native.config_files = vec!["other.json".to_owned()];
        sim.variants[0].launch.target = crate::types::ExecutionTarget::Wsl { distribution: "Ubuntu".to_owned() };
        sim.variants.push(native);
        accept_all(&mut project, &library).unwrap();
        project.members[1].variant_id = "native".to_owned();
        assert_eq!(check_project(&project, &library).unwrap().members[1].acceptance, AcceptanceState::Changed);
    }

    /// 手動の向き指定は、compact の不明な向きを上書きする。対応するチャネルがなければ問題になる。
    #[test]
    fn manual_access_overrides_unknown_directions() {
        let (_, project, mut library) = project("check-manual");
        let owner = library.get_mut("asset-owner").unwrap();
        owner.variants[0].manual_access = vec![crate::model::ManualAccess { robot: "Rig".to_owned(), channel_id: 0, access: Access::Read }];
        let variant = owner.variants[0].clone();
        let (entries, _) = effective_entries(&variant, &ActivationTiming::BeforeStart);
        let ports = member_ports(&variant, &entries);
        assert!(ports.access.iter().any(|a| a.channel.channel_id == 0 && a.access == Access::Read));
        assert!(ports.access.iter().any(|a| a.channel.channel_id == 1 && a.access == Access::Unknown));
        let mut missing = variant.clone();
        missing.manual_access[0].channel_id = 9;
        assert_eq!(member_ports(&missing, &entries).problems.len(), 1);
        let _ = project;
    }

    /// メンバーを足す前のプレビューは、保存せずに所見を返す。
    #[test]
    fn preview_reports_without_saving() {
        let (dir, project, mut library) = project("check-preview");
        legacy(&dir, "big.json", "Rig", &[(1, "attitude", 80)], &[]);
        let viewer = definition("viewer", ActivationTiming::AfterStart, &dir, vec!["big.json"]);
        let asset = library_asset_from_definition("asset-viewer".to_owned(), &viewer);
        library.insert(asset.id.clone(), asset.clone());
        let settings = crate::manage::MemberSettings::of(&member_from_definition(&viewer, asset.id));
        let preview = check_project(&crate::manage::with_new_member(&project, settings).unwrap(), &library).unwrap();
        assert!(preview.report.findings.iter().any(|f| f.code == "size_exceeds_owner"));
        assert_eq!(project.members.len(), 2);
    }

    /// 生成スクリプトが書いた gimbal-drone-demo の v1 を取り込むと、既定の構成は承認済みで
    /// Error がない。`HDM_GIMBAL_WORKSPACE` に launcher/workspace.v1.json を渡したときだけ実行する。
    #[test]
    fn gimbal_drone_demo_workspace_passes_the_start_checks() {
        let Some(path) = std::env::var_os("HDM_GIMBAL_WORKSPACE").map(PathBuf::from) else { return };
        let workspace: crate::types::Workspace = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let (assets, mut project) = crate::model::split_v1_workspace(&workspace);
        let library: Library = assets.into_iter().map(|asset| (asset.id.clone(), asset)).collect();
        accept_all(&mut project, &library).unwrap();
        let check = check_project(&project, &library).unwrap();
        for member in &check.members {
            println!("{} enabled={} entries={:?}", member.member_id, member.enabled, member.entries.iter().map(|e| (&e.path, e.usage)).collect::<Vec<_>>());
        }
        for finding in &check.report.findings {
            println!("{:?} {} {}", finding.severity, finding.code, finding.message);
        }
        assert!(check.blocking_reasons().is_empty(), "{:?}", check.blocking_reasons());
        let owned: usize = check.members.iter().filter(|m| m.enabled).flat_map(|m| m.entries.iter()).filter(|e| e.usage == Usage::Register).count();
        assert_eq!(owned, 2, "rig-owner と drone-mock が登録する");
    }
}
