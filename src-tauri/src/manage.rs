//! ライブラリとプロジェクトのメンバーを、画面から直接編集する操作。
//!
//! ライブラリのアセットは不変の版として扱う。編集すると新しい id のアセットになり
//! （`Store::stage_asset`）、プロジェクトのメンバーは利用者が選んだものだけ付け替える。
//! メンバーの編集は、画面が読み込んだときの内容（`expected`）と今の内容が同じときだけ
//! 受け付ける。ほかの操作で変わった内容を、古い表示のまま上書きしないため。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{Acceptance, LibraryAsset, Member, Project, STORE_SCHEMA_VERSION};
use crate::store::Store;
use crate::types::{ActivationTiming, AssetDefinition, ConnectionDefinition, CoreController, ReadinessCheck};

/// 画面から受け取るメンバーの設定。承認（`accepted`）は持たない。承認は、表示した
/// revision と照合する操作でだけ付ける。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberSettings {
    pub id: String,
    pub asset_id: String,
    pub variant_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub optional_depends_on: Vec<String>,
    #[serde(default)]
    pub activation_timing: Option<ActivationTiming>,
    #[serde(default)]
    pub readiness: Option<ReadinessCheck>,
    #[serde(default)]
    pub env_overrides: BTreeMap<String, String>,
}

impl MemberSettings {
    pub fn of(member: &Member) -> Self {
        Self {
            id: member.id.clone(),
            asset_id: member.asset_id.clone(),
            variant_id: member.variant_id.clone(),
            name: member.name.clone(),
            enabled: member.enabled,
            depends_on: member.depends_on.clone(),
            optional_depends_on: member.optional_depends_on.clone(),
            activation_timing: member.activation_timing.clone(),
            readiness: member.readiness.clone(),
            env_overrides: member.env_overrides.clone(),
        }
    }

    pub fn into_member(self, accepted: Option<Acceptance>) -> Member {
        Member {
            id: self.id,
            asset_id: self.asset_id,
            variant_id: self.variant_id,
            name: self.name,
            enabled: self.enabled,
            depends_on: self.depends_on,
            optional_depends_on: self.optional_depends_on,
            activation_timing: self.activation_timing,
            readiness: self.readiness,
            env_overrides: self.env_overrides,
            accepted,
        }
    }
}

/// ライブラリのアセット 1 つと、それを使っているメンバー。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub asset: LibraryAsset,
    pub used_by: Vec<AssetUser>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetUser {
    pub project_id: String,
    pub project_name: String,
    pub member_ids: Vec<String>,
}

/// ライブラリの一覧。名前順。
pub fn library_view(store: &Store) -> Vec<LibraryEntry> {
    let mut entries: Vec<LibraryEntry> = store
        .library
        .values()
        .map(|asset| LibraryEntry {
            asset: asset.clone(),
            used_by: store
                .projects
                .values()
                .filter_map(|project| {
                    let member_ids: Vec<String> = project.members.iter().filter(|member| member.asset_id == asset.id).map(|member| member.id.clone()).collect();
                    (!member_ids.is_empty()).then(|| AssetUser { project_id: project.id.clone(), project_name: project.name.clone(), member_ids })
                })
                .collect(),
        })
        .collect();
    entries.sort_by(|a, b| a.asset.name.cmp(&b.asset.name).then_with(|| a.asset.id.cmp(&b.asset.id)));
    entries
}

/// プロジェクトの `member_ids` を、`base_id` のアセットから新しい版に付け替える指示。
pub struct Apply<'a> {
    pub project_id: &'a str,
    pub member_ids: &'a [String],
}

/// アセットを保存し、保存した版の id を返す。
///
/// `base_id` があれば、それを編集した新しい版として置く（内容が同じなら `base_id` のまま）。
/// なければ新しいアセットとして足す。`apply` があれば、そのプロジェクトで `base_id` を
/// 使っているメンバーのうち指定されたものを付け替え、プロジェクトと一緒に保存する。
/// 付け替えたメンバーの承認はそのまま残るので、内容が変われば接続チェックで「変更あり」になる。
pub fn save_library_asset(store: &mut Store, mut asset: LibraryAsset, base_id: Option<&str>, apply: Option<Apply>) -> Result<String, String> {
    asset.schema_version = STORE_SCHEMA_VERSION;
    match base_id {
        Some(base) => {
            if !store.library.contains_key(base) {
                return Err("編集元のアセットがライブラリにありません。ライブラリを読み直してください。".to_owned());
            }
            asset.id = base.to_owned();
        }
        None => asset.id = Uuid::new_v4().to_string(),
    }
    let mut draft = store.draft();
    let id = Store::stage_asset(&mut draft, asset);
    let Some(apply) = apply else {
        store.commit_library(draft)?;
        return Ok(id);
    };
    let base = base_id.ok_or_else(|| "新しいアセットは、メンバーに付け替えられません。".to_owned())?;
    let mut project = store.projects.get(apply.project_id).cloned().ok_or_else(|| "そのプロジェクトはありません。".to_owned())?;
    for member_id in apply.member_ids {
        let member = project.members.iter_mut().find(|member| &member.id == member_id).ok_or_else(|| format!("メンバー「{member_id}」がありません。"))?;
        if member.asset_id != base {
            return Err(format!("メンバー「{member_id}」は、もうこのアセットを使っていません。画面を読み直してください。"));
        }
        member.asset_id = id.clone();
    }
    store.commit(draft, project)?;
    Ok(id)
}

/// メンバーの設定を置き換える。`expected` は画面が読み込んだときの設定で、今の設定と違えば拒否する。
/// アセットは変えられない（[`set_member_asset`] を使う）。承認は引き継ぐ。
pub fn update_member(store: &mut Store, project_id: &str, expected: &MemberSettings, settings: MemberSettings) -> Result<(), String> {
    let mut project = project_of(store, project_id)?;
    let member = project.members.iter_mut().find(|member| member.id == expected.id).ok_or_else(|| "そのメンバーはもうありません。".to_owned())?;
    ensure_unchanged(member, expected)?;
    if settings.id != expected.id || settings.asset_id != expected.asset_id {
        return Err("メンバーの ID とアセットは、この操作では変えられません。".to_owned());
    }
    *member = settings.into_member(member.accepted.clone());
    store.commit(store.draft(), project)
}

/// メンバーが使うアセットとバリエーションを選び直す。承認は引き継ぐので、内容が変われば
/// 接続チェックで「変更あり」になる。
pub fn set_member_asset(store: &mut Store, project_id: &str, member_id: &str, expected_asset_id: &str, expected_variant_id: &str, asset_id: &str, variant_id: &str) -> Result<(), String> {
    let mut project = project_of(store, project_id)?;
    let member = project.members.iter_mut().find(|member| member.id == member_id).ok_or_else(|| "そのメンバーはもうありません。".to_owned())?;
    if member.asset_id != expected_asset_id || member.variant_id != expected_variant_id {
        return Err("メンバーのアセットが、表示した後に変わっています。画面を読み直してください。".to_owned());
    }
    member.asset_id = asset_id.to_owned();
    member.variant_id = variant_id.to_owned();
    store.commit(store.draft(), project)
}

/// メンバーを外す。ほかのメンバーが依存していれば、プロジェクトの検証が拒否する。
pub fn remove_member(store: &mut Store, project_id: &str, expected: &MemberSettings) -> Result<(), String> {
    let mut project = project_of(store, project_id)?;
    let member = project.member(&expected.id).ok_or_else(|| "そのメンバーはもうありません。".to_owned())?;
    ensure_unchanged(member, expected)?;
    project.members.retain(|member| member.id != expected.id);
    store.commit(store.draft(), project)
}

/// メンバーを足した候補のプロジェクトを作る（保存しない）。承認はまだ付けない。
pub fn with_new_member(project: &Project, settings: MemberSettings) -> Result<Project, String> {
    if project.member(&settings.id).is_some() {
        return Err(format!("メンバー ID「{}」は、このプロジェクトで使われています。", settings.id));
    }
    let mut candidate = project.clone();
    candidate.members.push(settings.into_member(None));
    Ok(candidate)
}

pub fn rename_project(store: &mut Store, project_id: &str, expected: &str, name: &str) -> Result<(), String> {
    let mut project = project_of(store, project_id)?;
    if project.name != expected {
        return Err("プロジェクト名が、表示した後に変わっています。画面を読み直してください。".to_owned());
    }
    project.name = name.trim().to_owned();
    store.commit(store.draft(), project)
}

pub fn set_project_description(store: &mut Store, project_id: &str, expected: &str, description: &str) -> Result<(), String> {
    let mut project = project_of(store, project_id)?;
    if project.description != expected {
        return Err("プロジェクトの概要が、表示した後に変わっています。画面を読み直してください。".to_owned());
    }
    project.description = description.trim().to_owned();
    store.commit(store.draft(), project)
}

pub fn set_core_controller(store: &mut Store, project_id: &str, expected: Option<&CoreController>, controller: Option<CoreController>) -> Result<(), String> {
    let mut project = project_of(store, project_id)?;
    if serde_json::to_value(&project.core_controller).ok() != serde_json::to_value(expected).ok() {
        return Err("Core コントローラーの設定が、表示した後に変わっています。画面を読み直してください。".to_owned());
    }
    project.core_controller = controller;
    store.commit(store.draft(), project)
}

/// 取り込んだ定義を既存のプロジェクトに足すために、`taken` と重なる id を振り直す（`-2`、`-3` …）。
/// 取り込んだ定義どうしの依存と、接続の端点・観測者も、振り直した id に合わせる。振り直した後の
/// id を、取り込んだ順に返す。
pub fn renumber_imported(taken: &std::collections::BTreeSet<String>, assets: &mut [AssetDefinition], connections: &mut [ConnectionDefinition]) -> Vec<String> {
    let mut used = taken.clone();
    used.insert(crate::model::CORE_SENTINEL.to_owned());
    let mut map = BTreeMap::new();
    for asset in assets.iter_mut() {
        let mut id = asset.id.clone();
        let mut n = 2;
        while used.contains(&id) {
            id = format!("{}-{n}", asset.id);
            n += 1;
        }
        used.insert(id.clone());
        map.insert(asset.id.clone(), id.clone());
        asset.id = id;
    }
    let follow = |id: &mut String| {
        if let Some(new) = map.get(id) {
            *id = new.clone();
        }
    };
    for asset in assets.iter_mut() {
        asset.depends_on.iter_mut().for_each(follow);
        asset.optional_depends_on.iter_mut().for_each(follow);
    }
    for connection in connections.iter_mut() {
        for slot in [&mut connection.source_asset_id, &mut connection.destination_asset_id, &mut connection.owner_asset_id] {
            if let Some(id) = slot.as_mut() {
                follow(id);
            }
        }
    }
    assets.iter().map(|asset| asset.id.clone()).collect()
}

fn project_of(store: &Store, project_id: &str) -> Result<Project, String> {
    store.projects.get(project_id).cloned().ok_or_else(|| "そのプロジェクトはありません。".to_owned())
}

fn ensure_unchanged(member: &Member, expected: &MemberSettings) -> Result<(), String> {
    if serde_json::to_value(MemberSettings::of(member)).ok() != serde_json::to_value(expected).ok() {
        return Err("メンバーの設定が、表示した後に変わっています。画面を読み直してから編集してください。".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::tests::v1_demo;
    use std::fs;

    fn store_with_demo(name: &str) -> (Store, String) {
        let dir = std::env::temp_dir().join(format!("hdm-manage-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(crate::store::V1_WORKSPACE_FILE), serde_json::to_vec(&v1_demo()).unwrap()).unwrap();
        let store = Store::open(&dir);
        let id = store.active_project_id().to_owned();
        (store, id)
    }

    fn member_asset(store: &Store, project: &str, member: &str) -> String {
        store.projects[project].member(member).unwrap().asset_id.clone()
    }

    /// ライブラリだけに保存すると、プロジェクトは元の版を使い続ける。
    #[test]
    fn saving_to_the_library_leaves_projects_alone() {
        let (mut store, id) = store_with_demo("lib-only");
        let base = member_asset(&store, &id, "pilot");
        let mut asset = store.library[&base].clone();
        asset.variants[0].launch.args.push("--edited".to_owned());
        let new_id = save_library_asset(&mut store, asset, Some(&base), None).unwrap();
        assert_ne!(new_id, base);
        assert_eq!(member_asset(&store, &id, "pilot"), base);
        assert!(store.library.contains_key(&base) && store.library.contains_key(&new_id));
        // 内容が同じなら、新しい版を作らない。
        let same = store.library[&new_id].clone();
        assert_eq!(save_library_asset(&mut store, same, Some(&new_id), None).unwrap(), new_id);
    }

    /// 反映を選んだメンバーだけが新しい版に付け替わり、承認は残るので「変更あり」になる。
    #[test]
    fn applying_moves_only_the_chosen_members_and_needs_acceptance() {
        let (mut store, id) = store_with_demo("apply");
        let base = member_asset(&store, &id, "pilot");
        let mut asset = store.library[&base].clone();
        asset.variants[0].launch.args.push("--edited".to_owned());
        let new_id = save_library_asset(&mut store, asset, Some(&base), Some(Apply { project_id: &id, member_ids: &["pilot".to_owned()] })).unwrap();
        assert_eq!(member_asset(&store, &id, "pilot"), new_id);
        // ほかに使っていない元の版は片付けられる。
        assert!(!store.library.contains_key(&base));
        let check = crate::check::check_project(&store.projects[&id], &store.library).unwrap();
        let pilot = check.members.iter().find(|m| m.member_id == "pilot").unwrap();
        assert!(pilot.acceptance != crate::check::AcceptanceState::Accepted, "{:?}", pilot.acceptance);
    }

    /// 別のアセットを使っているメンバーを付け替えようとしたら、何も保存しない。
    #[test]
    fn applying_to_a_member_on_another_asset_is_refused() {
        let (mut store, id) = store_with_demo("apply-other");
        let base = member_asset(&store, &id, "pilot");
        let before = store.library.len();
        let mut asset = store.library[&base].clone();
        asset.variants[0].launch.args.push("--edited".to_owned());
        let result = save_library_asset(&mut store, asset, Some(&base), Some(Apply { project_id: &id, member_ids: &["rig-sim".to_owned()] }));
        assert!(result.is_err());
        assert_eq!(store.library.len(), before);
        assert_eq!(member_asset(&store, &id, "pilot"), base);
    }

    /// 画面が読み込んだ後に変わったメンバーは、古い表示のまま上書きしない。承認は受け取らない。
    #[test]
    fn member_updates_check_the_expected_settings() {
        let (mut store, id) = store_with_demo("update");
        let expected = MemberSettings::of(store.projects[&id].member("pilot").unwrap());
        let accepted = store.projects[&id].member("pilot").unwrap().accepted.clone();
        let mut edited = expected.clone();
        edited.enabled = !expected.enabled;
        update_member(&mut store, &id, &expected, edited.clone()).unwrap();
        let member = store.projects[&id].member("pilot").unwrap();
        assert_eq!(member.enabled, edited.enabled);
        assert_eq!(member.accepted, accepted);
        // 古い expected のままでは拒否する。
        assert!(update_member(&mut store, &id, &expected, expected.clone()).is_err());
        // 承認つきの JSON を送っても、承認は入力に入らない。
        let with_acceptance = serde_json::json!({ "id": "x", "asset_id": "a", "variant_id": "v", "accepted": { "revision": "r", "manifest": [], "accepted_at": "2026-10-23T00:00:00Z" } });
        let parsed: MemberSettings = serde_json::from_value(with_acceptance).unwrap();
        assert!(parsed.into_member(None).accepted.is_none());
    }

    /// 依存されているメンバーは外せない。外せるものは外れ、アセットはライブラリに残る。
    #[test]
    fn removing_members() {
        let (mut store, id) = store_with_demo("remove");
        let rig_sim = MemberSettings::of(store.projects[&id].member("rig-sim").unwrap());
        assert!(remove_member(&mut store, &id, &rig_sim).is_err(), "pilot が rig-sim に依存している");
        let mujoco = MemberSettings::of(store.projects[&id].member("viz-mujoco").unwrap());
        let pilot = store.projects[&id].member("pilot").unwrap().clone();
        let mut pilot_settings = MemberSettings::of(&pilot);
        pilot_settings.optional_depends_on.retain(|d| d != "viz-mujoco");
        update_member(&mut store, &id, &MemberSettings::of(&pilot), pilot_settings).unwrap();
        remove_member(&mut store, &id, &mujoco).unwrap();
        assert!(store.projects[&id].member("viz-mujoco").is_none());
        assert!(store.library.contains_key(&mujoco.asset_id));
        // 使われなくなったアセットは消せる。使われているものは消せない。
        assert!(store.remove_asset(&member_asset(&store, &id, "pilot")).is_err());
        store.remove_asset(&mujoco.asset_id).unwrap();
        assert!(!store.library.contains_key(&mujoco.asset_id));
    }

    /// 既存のプロジェクトに足すときは、重なる id を振り直し、依存と接続もそれに合わせる。
    #[test]
    fn renumbering_imported_definitions() {
        let workspace = v1_demo();
        let taken: std::collections::BTreeSet<String> = workspace.assets.iter().map(|asset| asset.id.clone()).collect();
        let mut assets = workspace.assets.clone();
        let mut connections = vec![ConnectionDefinition { id: "c".to_owned(), source: "s".to_owned(), destination: "d".to_owned(), label: String::new(), transport: crate::types::TransportKind::Unknown, pdu_names: Vec::new(), endpoint_config: None, details: Default::default(), source_asset_id: Some("pilot".to_owned()), destination_asset_id: Some("rig-sim".to_owned()), owner_asset_id: None }];
        let ids = renumber_imported(&taken, &mut assets, &mut connections);
        assert!(ids.iter().all(|id| !taken.contains(id) && id != "core"), "{ids:?}");
        let pilot = assets.iter().find(|asset| asset.id == "pilot-2").unwrap();
        assert!(pilot.depends_on.contains(&"rig-sim-2".to_owned()), "{:?}", pilot.depends_on);
        assert!(pilot.depends_on.iter().all(|dependency| dependency != "rig-sim"));
        assert_eq!(connections[0].source_asset_id.as_deref(), Some("pilot-2"));
        assert_eq!(connections[0].destination_asset_id.as_deref(), Some("rig-sim-2"));
    }

    /// アセットの選び直しは、表示したときのアセットと違えば拒否する。
    #[test]
    fn switching_a_member_asset_checks_the_expected_asset() {
        let (mut store, id) = store_with_demo("switch");
        let pilot = member_asset(&store, &id, "pilot");
        let other = member_asset(&store, &id, "viz-foxglove");
        assert!(set_member_asset(&mut store, &id, "pilot", &other, "default", &other, "default").is_err());
        assert!(set_member_asset(&mut store, &id, "pilot", &pilot, "other-variant", &other, "default").is_err(), "バリエーションも照合する");
        set_member_asset(&mut store, &id, "pilot", &pilot, "default", &other, "default").unwrap();
        assert_eq!(member_asset(&store, &id, "pilot"), other);
        assert!(set_member_asset(&mut store, &id, "pilot", &other, "default", &other, "missing").is_err(), "ないバリエーションは検証で拒否");
    }

    /// プロジェクト名と Core コントローラーも、表示した後に変わっていれば上書きしない。
    #[test]
    fn project_fields_check_the_expected_values() {
        let (mut store, id) = store_with_demo("fields");
        let name = store.projects[&id].name.clone();
        rename_project(&mut store, &id, &name, "Renamed").unwrap();
        assert!(rename_project(&mut store, &id, &name, "Again").is_err());
        set_project_description(&mut store, &id, "", "  1 つの操縦入力で 2 機を動かす。\n").unwrap();
        assert_eq!(store.projects[&id].description, "1 つの操縦入力で 2 機を動かす。");
        assert!(set_project_description(&mut store, &id, "", "Again").is_err());
        let current = store.projects[&id].core_controller.clone();
        let next = CoreController { id: "controller-e2e".to_owned(), name: "C".to_owned(), command: crate::types::ProgramSpec { program: "x".to_owned(), args: Vec::new(), cwd: None, env: BTreeMap::new(), target: Default::default() }, readiness: Default::default() };
        set_core_controller(&mut store, &id, current.as_ref(), Some(next.clone())).unwrap();
        assert!(set_core_controller(&mut store, &id, current.as_ref(), None).is_err());
        set_core_controller(&mut store, &id, Some(&next), None).unwrap();
    }
}
