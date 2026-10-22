//! 画面が編集した `Workspace` を、プロジェクトとライブラリに書き戻す。
//!
//! 既存の画面は解決済みの `Workspace` を丸ごと編集して送ってくる。その差分を、
//! メンバーで決まること（一括起動の対象・依存・起動タイミング・準備完了の条件・
//! 環境の上書き）と、アセットで決まること（名前・役割・起動コマンド・設定ファイル）
//! に振り分ける。アセットへの書き込みはコピーオンライトで、ほかのプロジェクトが
//! 使っているアセットは複製してから変える。

use std::collections::BTreeMap;

use uuid::Uuid;

use crate::model::{library_asset_from_definition, member_from_definition, Member, Variant};
use crate::store::Store;
use crate::types::{AssetDefinition, Workspace};

/// 画面で新しく足したメンバーのアセットの id。同じプロジェクトに同じ id で取り込み直すと
/// 同じになり、ライブラリが増えない。
fn new_member_asset_id(project_id: &str, member_id: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("hakoniwa-desktop-manager:member-asset:{project_id}:{member_id}").as_bytes()).to_string()
}

/// `workspace` の内容で、プロジェクト `project_id` を置き換える。
pub fn apply_workspace(store: &mut Store, project_id: &str, workspace: &Workspace) -> Result<(), String> {
    workspace.validate()?;
    let mut project = store.projects.get(project_id).cloned().ok_or_else(|| "編集するプロジェクトがありません。".to_owned())?;
    project.name = workspace.name.clone();
    project.source_directory = workspace.source_directory.clone();
    project.core_release = workspace.core_release.clone();
    project.core_controller = workspace.core_controller.clone();
    project.core_env = workspace.core_env.clone();
    project.connections = workspace.imported_connections.clone();

    let mut members = Vec::with_capacity(workspace.assets.len());
    for definition in &workspace.assets {
        let member = match project.member(&definition.id).cloned() {
            Some(member) => update_member(store, project_id, member, definition)?,
            None => {
                let asset = library_asset_from_definition(new_member_asset_id(project_id, &definition.id), definition);
                let asset_id = store.write_asset_for(asset, project_id)?;
                member_from_definition(definition, asset_id)
            }
        };
        members.push(member);
    }
    project.members = members;
    project.validate(&store.library)?;
    store.save_project(&project)?;
    store.projects.insert(project.id.clone(), project);
    Ok(())
}

fn update_member(store: &mut Store, project_id: &str, mut member: Member, definition: &AssetDefinition) -> Result<Member, String> {
    let asset = store.library.get(&member.asset_id).cloned().ok_or_else(|| format!("メンバー「{}」のアセットがライブラリにありません。", member.id))?;
    let variant = asset.variant(&member.variant_id).cloned().ok_or_else(|| format!("メンバー「{}」のバリエーションがありません。", member.id))?;

    member.enabled = definition.enabled;
    member.depends_on = definition.depends_on.clone();
    member.optional_depends_on = definition.optional_depends_on.clone();
    // バリエーションの既定値と同じなら上書きを持たない。既定値を後から変えたときに追従させる。
    member.activation_timing = (definition.activation_timing != variant.activation_timing).then(|| definition.activation_timing.clone());
    member.readiness = (json(&definition.readiness) != json(&variant.readiness)).then(|| definition.readiness.clone());

    let mut edited = asset.clone();
    match &member.name {
        // 表示名を持つメンバー（同じアセットの 2 つ目など）は、名前の変更をメンバーに入れる。
        Some(_) => member.name = (definition.name != asset.name).then(|| definition.name.clone()),
        None if definition.name != asset.name => edited.name = definition.name.clone(),
        None => {}
    }
    edited.role = definition.role.clone();

    // 環境変数は、メンバーが上書きしている名前ならメンバーへ、それ以外はアセットへ戻す。
    let mut launch_env = BTreeMap::new();
    let mut overrides = BTreeMap::new();
    for (name, value) in &definition.command.env {
        if member.env_overrides.contains_key(name) {
            overrides.insert(name.clone(), value.clone());
        } else {
            launch_env.insert(name.clone(), value.clone());
        }
    }
    member.env_overrides = overrides;
    let mut launch = definition.command.clone();
    launch.env = launch_env;
    let edited_variant = Variant { launch, config_files: definition.config_files.clone(), ..variant };
    for slot in edited.variants.iter_mut() {
        if slot.id == edited_variant.id {
            *slot = edited_variant.clone();
        }
    }
    if json(&without_timestamp(&edited)) != json(&without_timestamp(&asset)) {
        member.asset_id = store.write_asset_for(edited, project_id)?;
    }
    Ok(member)
}

fn json<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_default()
}

fn without_timestamp(asset: &crate::model::LibraryAsset) -> serde_json::Value {
    let mut value = json(asset);
    if let Some(object) = value.as_object_mut() {
        object.remove("updated_at");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::resolve;
    use crate::resolve::tests::v1_demo;
    use crate::types::ActivationTiming;
    use std::fs;

    fn store_with_demo(name: &str) -> (Store, String) {
        let dir = std::env::temp_dir().join(format!("hdm-edit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(crate::store::V1_WORKSPACE_FILE), serde_json::to_vec(&v1_demo()).unwrap()).unwrap();
        let store = Store::open(&dir);
        let id = store.active_project_id().to_owned();
        (store, id)
    }

    fn resolved(store: &Store, id: &str) -> Workspace {
        resolve(&store.projects[id], &store.library).unwrap()
    }

    /// 解決した結果をそのまま書き戻しても、プロジェクトもライブラリも変わらない。
    #[test]
    fn writing_back_an_unchanged_workspace_changes_nothing() {
        let (mut store, id) = store_with_demo("noop");
        let before_project = json(&store.projects[&id]);
        let before_library: Vec<_> = store.library.values().map(without_timestamp).collect();
        let workspace = resolved(&store, &id);
        apply_workspace(&mut store, &id, &workspace).unwrap();
        assert_eq!(json(&store.projects[&id]), before_project);
        assert_eq!(store.library.values().map(without_timestamp).collect::<Vec<_>>(), before_library);
    }

    /// メンバーの項目はメンバーに、起動コマンドはアセットに入る。既定値と同じ起動タイミングは上書きにしない。
    #[test]
    fn edits_go_to_the_member_or_the_asset() {
        let (mut store, id) = store_with_demo("split");
        let mut workspace = resolved(&store, &id);
        let pilot = workspace.assets.iter_mut().find(|a| a.id == "pilot").unwrap();
        pilot.enabled = false;
        pilot.activation_timing = ActivationTiming::Manual;
        pilot.command.args.push("--verbose".to_owned());
        let rig_sim = workspace.assets.iter_mut().find(|a| a.id == "rig-sim").unwrap();
        rig_sim.activation_timing = ActivationTiming::AfterStart; // 既定値と同じ
        apply_workspace(&mut store, &id, &workspace).unwrap();

        let project = &store.projects[&id];
        let member = project.member("pilot").unwrap();
        assert!(!member.enabled);
        assert_eq!(member.activation_timing, Some(ActivationTiming::Manual));
        assert!(store.library[&member.asset_id].variants[0].launch.args.contains(&"--verbose".to_owned()));
        assert_eq!(project.member("rig-sim").unwrap().activation_timing, None);
        assert_eq!(json(&resolved(&store, &id)), json(&workspace));
    }

    /// 環境変数は、メンバーが上書きしているものだけメンバーに戻る。
    #[test]
    fn env_edits_follow_member_overrides() {
        let (mut store, id) = store_with_demo("env");
        let member = store.projects.get_mut(&id).unwrap().members.iter_mut().find(|m| m.id == "viz-foxglove").unwrap();
        member.env_overrides.insert("FOXGLOVE_PORT".to_owned(), "8791".to_owned());
        let mut workspace = resolved(&store, &id);
        let viewer = workspace.assets.iter_mut().find(|a| a.id == "viz-foxglove").unwrap();
        viewer.command.env.insert("FOXGLOVE_PORT".to_owned(), "8792".to_owned());
        viewer.command.env.insert("PYTHONUNBUFFERED".to_owned(), "0".to_owned());
        apply_workspace(&mut store, &id, &workspace).unwrap();
        let member = store.projects[&id].member("viz-foxglove").unwrap();
        assert_eq!(member.env_overrides.get("FOXGLOVE_PORT").map(String::as_str), Some("8792"));
        assert!(!member.env_overrides.contains_key("PYTHONUNBUFFERED"));
        let launch = &store.library[&member.asset_id].variants[0].launch;
        assert_eq!(launch.env.get("PYTHONUNBUFFERED").map(String::as_str), Some("0"));
        assert!(!launch.env.contains_key("FOXGLOVE_PORT"));
    }

    /// ほかのプロジェクトも使うアセットを編集すると、複製に付け替え、ほかのプロジェクトは変わらない。
    #[test]
    fn editing_a_shared_asset_leaves_the_other_project_alone() {
        let (mut store, id) = store_with_demo("shared");
        let mut other = store.projects[&id].clone();
        other.id = "other".to_owned();
        other.origin = None;
        store.save_project(&other).unwrap();
        store.projects.insert(other.id.clone(), other);
        let other_before = json(&resolved(&store, "other"));

        let mut workspace = resolved(&store, &id);
        workspace.assets[0].command.args.push("--edited".to_owned());
        apply_workspace(&mut store, &id, &workspace).unwrap();
        assert_eq!(json(&resolved(&store, "other")), other_before);
        assert_ne!(store.projects[&id].members[0].asset_id, store.projects["other"].members[0].asset_id);
        assert!(resolved(&store, &id).assets[0].command.args.contains(&"--edited".to_owned()));
    }

    /// 足したアセットは新しいメンバーとアセットになり、消したものはメンバーから外れる（アセットは残る）。
    #[test]
    fn adding_and_removing_members() {
        let (mut store, id) = store_with_demo("add-remove");
        let mut workspace = resolved(&store, &id);
        let removed_asset = store.projects[&id].member("viz-mujoco").unwrap().asset_id.clone();
        workspace.assets.retain(|a| a.id != "viz-mujoco");
        workspace.assets.iter_mut().for_each(|a| a.optional_depends_on.retain(|d| d != "viz-mujoco"));
        let mut extra = workspace.assets[0].clone();
        extra.id = "extra".to_owned();
        extra.name = "Extra".to_owned();
        extra.depends_on = vec!["core".to_owned()];
        workspace.assets.push(extra);
        apply_workspace(&mut store, &id, &workspace).unwrap();
        let project = &store.projects[&id];
        assert!(project.member("viz-mujoco").is_none());
        assert!(store.library.contains_key(&removed_asset));
        let added = project.member("extra").unwrap();
        assert_eq!(store.library[&added.asset_id].name, "Extra");
        assert_eq!(json(&resolved(&store, &id)), json(&workspace));
        // 同じ内容で書き戻しても、足したアセットは増えない。
        let count = store.library.len();
        apply_workspace(&mut store, &id, &workspace).unwrap();
        assert_eq!(store.library.len(), count);
    }
}
