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
use crate::model::{Acceptance, Library, LibraryAsset, Member, PduEntrySpec, Project, Variant, CORE_NAMESPACE_VARIABLE};
use crate::ports::{self, FileDigest, PduEntry, PortSet, Usage};
use crate::types::ActivationTiming;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCheck {
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
                AcceptanceState::NotAccepted => reasons.push(format!("{} の PDU 設定はまだ承認されていません。接続チェックで確認して承認してください。", member.name)),
                AcceptanceState::NoPduConfig | AcceptanceState::Accepted => {}
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
pub fn effective_entries(variant: &Variant, timing: &ActivationTiming) -> (Vec<PduEntrySpec>, bool) {
    if !variant.pdu_entries.is_empty() {
        return (variant.pdu_entries.clone(), false);
    }
    let mut entries = Vec::new();
    for file in &variant.config_files {
        let Ok(bytes) = std::fs::read(resolve_entry_path(file, variant)) else { continue };
        let Ok(value) = serde_json::from_str::<Value>(String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}')) else { continue };
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
fn member_ports(variant: &Variant, entries: &[PduEntrySpec]) -> PortSet {
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
        changes.push("起動定義が変わった".to_owned());
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
    let (entries, inferred) = effective_entries(variant, &timing);
    let ports = member_ports(variant, &entries);
    let mut manifest = ports.files.clone();
    manifest.extend(extra_digests(variant));
    let revision = revision(variant, member, &entries, &manifest, &ports.problems);
    let (acceptance, changes) = if entries.is_empty() {
        (AcceptanceState::NoPduConfig, Vec::new())
    } else {
        match &member.accepted {
            None => (AcceptanceState::NotAccepted, Vec::new()),
            Some(accepted) if accepted.revision == revision => (AcceptanceState::Accepted, Vec::new()),
            Some(accepted) => (AcceptanceState::Changed, describe_changes(accepted, &manifest)),
        }
    };
    let name = member.name.clone().unwrap_or_else(|| asset.name.clone());
    let mut env = variant.launch.env.clone();
    env.extend(member.env_overrides.clone());
    Ok(Evaluated {
        ports: MemberPorts { member_id: member.id.clone(), name: name.clone(), activation_timing: timing, hako_config_path: env.get(CORE_NAMESPACE_VARIABLE).cloned(), ports },
        check: MemberCheck { member_id: member.id.clone(), name, enabled: member.enabled, entries, inferred, revision, manifest, acceptance, changes },
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
    let report = compat::check(&ports, project.core_env.get(CORE_NAMESPACE_VARIABLE).map(String::as_str));
    Ok(ProjectCheck { report, members })
}

/// `member_ids` のメンバーを、今の内容で承認する。
pub fn accept_members(project: &mut Project, library: &Library, member_ids: &[String]) -> Result<(), String> {
    for id in member_ids {
        let member = project.members.iter_mut().find(|member| &member.id == id).ok_or_else(|| format!("メンバー「{id}」がありません。"))?;
        let asset = library.get(&member.asset_id).ok_or_else(|| format!("メンバー「{id}」のアセットがライブラリにありません。"))?;
        let evaluated = evaluate(member, asset)?;
        member.accepted = Some(Acceptance { revision: evaluated.check.revision, manifest: evaluated.check.manifest, accepted_at: Utc::now() });
    }
    Ok(())
}

/// すべてのメンバーを承認する。取り込みや移行のように、利用者が構成を明示的に入れた
/// ときに使う。
pub fn accept_all(project: &mut Project, library: &Library) -> Result<(), String> {
    let ids: Vec<String> = project.members.iter().map(|member| member.id.clone()).collect();
    accept_members(project, library, &ids)
}

/// メンバーを足したらどうなるかを、保存せずに確かめる。
pub fn preview_add(project: &Project, library: &Library, member: Member) -> Result<ProjectCheck, String> {
    let mut candidate = project.clone();
    candidate.members.push(member);
    check_project(&candidate, library)
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

    /// 起動定義（引数）を変えても、ファイルが同じなら「起動定義が変わった」として止める。
    #[test]
    fn launch_changes_also_need_acceptance() {
        let (_, mut project, mut library) = project("check-launch");
        accept_all(&mut project, &library).unwrap();
        library.get_mut("asset-sim").unwrap().variants[0].launch.args.push("--legs".to_owned());
        let check = check_project(&project, &library).unwrap();
        let sim = check.members.iter().find(|m| m.member_id == "sim").unwrap();
        assert_eq!(sim.acceptance, AcceptanceState::Changed);
        assert_eq!(sim.changes, vec!["起動定義が変わった"]);
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
    fn members_without_pdu_config_are_not_gated() {
        let (_, mut project, mut library) = project("check-none");
        library.get_mut("asset-sim").unwrap().variants[0].config_files = vec!["other.json".to_owned()];
        accept_members(&mut project, &library, &["owner".to_owned()]).unwrap();
        let check = check_project(&project, &library).unwrap();
        assert_eq!(check.members[1].acceptance, AcceptanceState::NoPduConfig);
        assert!(check.blocking_reasons().is_empty());
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
        let preview = preview_add(&project, &library, member_from_definition(&viewer, asset.id)).unwrap();
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
