//! プロジェクトとライブラリから、実行層と画面が使う `Workspace` を組み立てる。

// 呼び出し側（AppState の置き換え）は次のコミットで入る。
#![cfg_attr(not(test), allow(dead_code))]

use crate::model::{Library, Project};
use crate::types::{AssetDefinition, Workspace, WORKSPACE_SCHEMA_VERSION};

/// プロジェクトを解決する。メンバー 1 つがアセット定義 1 つになり、順序は保つ。
pub fn resolve(project: &Project, library: &Library) -> Result<Workspace, String> {
    project.validate(library)?;
    let mut assets = Vec::with_capacity(project.members.len());
    for member in &project.members {
        // validate で実在を確かめてあるので、ここでは見つかる。
        let asset = &library[&member.asset_id];
        let variant = asset.variant(&member.variant_id).expect("validated variant");
        let mut command = variant.launch.clone();
        command.env.extend(member.env_overrides.clone());
        assets.push(AssetDefinition {
            id: member.id.clone(),
            name: member.name.clone().unwrap_or_else(|| asset.name.clone()),
            role: asset.role.clone(),
            command,
            depends_on: member.depends_on.clone(),
            optional_depends_on: member.optional_depends_on.clone(),
            activation_timing: member.activation_timing.clone().unwrap_or_else(|| variant.activation_timing.clone()),
            config_files: variant.config_files.clone(),
            enabled: member.enabled,
            readiness: member.readiness.clone().unwrap_or_else(|| variant.readiness.clone()),
        });
    }
    let workspace = Workspace {
        schema_version: WORKSPACE_SCHEMA_VERSION,
        id: project.id.clone(),
        name: project.name.clone(),
        source_directory: project.source_directory.clone(),
        core_release: project.core_release.clone(),
        core_controller: project.core_controller.clone(),
        core_env: project.core_env.clone(),
        assets,
        imported_connections: project.connections.clone(),
        last_opened_at: project.last_opened_at,
    };
    workspace.validate()?;
    Ok(workspace)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::{split_v1_workspace, Member, CORE_SENTINEL, DEFAULT_VARIANT};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

    /// gimbal-drone-demo の make-workspace.ps1 が書く形の v1 ワークスペース（値は合成）。
    pub(crate) fn v1_demo() -> Workspace {
        let python = "C:/miniconda3/python.exe";
        let env = json!({ "HAKO_CONFIG_PATH": "C:/demo/cpp_core_config.json", "PYTHONUNBUFFERED": "1" });
        let asset = |id: &str, name: &str, role: &str, args: Value, timing: &str, deps: Value, optional: Value, enabled: bool, readiness: Value, configs: Value| {
            json!({
                "id": id, "name": name, "role": role,
                "command": { "program": python, "args": args, "cwd": "C:/demo", "env": env, "target": "native" },
                "depends_on": deps, "optional_depends_on": optional, "activation_timing": timing,
                "config_files": configs, "enabled": enabled, "readiness": readiness,
            })
        };
        serde_json::from_value(json!({
            "schema_version": 1, "id": "3f1c1f9e-0000-4000-8000-000000000001", "name": "Gimbal x Drone demo",
            "source_directory": "C:/demo",
            "core_release": { "version": "1.4.0", "install_directory": "C:/core", "hako_cmd_path": "C:/core/hako-cmd.exe", "verified_sha256": "abc", "installed_at": "2026-10-01T00:00:00Z" },
            "core_controller": null,
            "core_env": { "HAKO_CONFIG_PATH": "C:/demo/cpp_core_config.json" },
            "assets": [
                asset("rig-owner", "Gimbal rig (owner)", "simulator", json!(["src/rig_owner.py", "--conductor"]), "before_start", json!(["core"]), json!([]), true, json!({ "kind": "log_contains", "text": "REGISTERED" }), json!(["config/custom.json", "config/pdutypes.json"])),
                asset("drone-mock", "Free-flight drone", "simulator", json!(["src/mock_drone.py"]), "before_start", json!(["rig-owner"]), json!([]), true, json!({ "kind": "log_contains", "text": "REGISTERED" }), json!(["config/drone_mock_custom.json"])),
                asset("rig-sim", "Gimbal rig (MuJoCo)", "simulator", json!(["src/rig_sim.py"]), "after_start", json!(["rig-owner"]), json!([]), true, json!({ "kind": "manual" }), json!(["config/pdu_config_rig.json"])),
                asset("viz-foxglove", "Visualization (Foxglove)", "visualizer", json!(["src/viz_foxglove.py", "--legs", "full"]), "after_start", json!(["rig-sim"]), json!([]), true, json!({ "kind": "tcp_port", "host": "127.0.0.1", "port": 8791 }), json!(["config/pdu_config_viewer_full.json"])),
                asset("viz-mujoco", "Visualization (MuJoCo)", "visualizer", json!(["src/viz_mujoco.py", "--legs", "full"]), "after_start", json!(["rig-sim"]), json!([]), false, json!({ "kind": "manual" }), json!(["config/pdu_config_viewer_full.json"])),
                asset("pilot", "Pilot", "controller", json!(["src/pilot.py", "--legs", "full"]), "after_start", json!(["rig-owner", "drone-mock", "rig-sim"]), json!(["viz-foxglove", "viz-mujoco"]), true, json!({ "kind": "manual" }), json!(["config/pdu_config_pilot_full.json"])),
            ],
            "imported_connections": [
                { "id": "c1", "source": "Pilot", "destination": "ws://127.0.0.1:8791", "label": "viewer", "transport": "websocket", "pdu_names": [], "details": {} }
            ],
            "last_opened_at": "2026-10-22T00:00:00Z",
        }))
        .unwrap()
    }

    fn library_of(assets: Vec<crate::model::LibraryAsset>) -> Library {
        assets.into_iter().map(|asset| (asset.id.clone(), asset)).collect()
    }

    /// v1 を分けてから解決すると、元のワークスペースに戻る（Core、env、接続、id、順序も含めて）。
    #[test]
    fn split_then_resolve_reproduces_the_v1_workspace() {
        let original = v1_demo();
        let (assets, project) = split_v1_workspace(&original);
        let resolved = resolve(&project, &library_of(assets)).unwrap();
        assert_eq!(serde_json::to_value(&resolved).unwrap(), serde_json::to_value(&original).unwrap());
    }

    /// 同じアセットを 2 つのメンバーで使うと、表示名・依存・環境の上書きはメンバーごとに分かれる。
    #[test]
    fn one_asset_can_back_two_members() {
        let (assets, mut project) = split_v1_workspace(&v1_demo());
        let viewer = project.member("viz-foxglove").unwrap().clone();
        project.members.push(Member {
            id: "viz-foxglove-2".to_owned(),
            name: Some("Visualization (Foxglove, 2nd)".to_owned()),
            env_overrides: BTreeMap::from([("FOXGLOVE_PORT".to_owned(), "8792".to_owned())]),
            depends_on: vec!["rig-owner".to_owned()],
            enabled: false,
            ..viewer.clone()
        });
        let resolved = resolve(&project, &library_of(assets)).unwrap();
        let first = resolved.assets.iter().find(|a| a.id == "viz-foxglove").unwrap();
        let second = resolved.assets.iter().find(|a| a.id == "viz-foxglove-2").unwrap();
        assert_eq!(first.name, "Visualization (Foxglove)");
        assert_eq!(second.name, "Visualization (Foxglove, 2nd)");
        assert_eq!(second.depends_on, vec!["rig-owner"]);
        assert_eq!(second.command.env.get("FOXGLOVE_PORT").map(String::as_str), Some("8792"));
        assert!(!first.command.env.contains_key("FOXGLOVE_PORT"));
        assert_eq!(first.command.args, second.command.args);
    }

    /// メンバーの起動タイミングと準備完了の条件は、バリエーションの既定値を上書きする。
    #[test]
    fn member_overrides_win_over_variant_defaults() {
        let (assets, mut project) = split_v1_workspace(&v1_demo());
        let member = project.members.iter_mut().find(|m| m.id == "rig-sim").unwrap();
        member.activation_timing = Some(crate::types::ActivationTiming::Manual);
        member.readiness = Some(crate::types::ReadinessCheck::LogContains { text: "READY".to_owned() });
        let resolved = resolve(&project, &library_of(assets)).unwrap();
        let rig_sim = resolved.assets.iter().find(|a| a.id == "rig-sim").unwrap();
        assert_eq!(rig_sim.activation_timing, crate::types::ActivationTiming::Manual);
        assert!(matches!(&rig_sim.readiness, crate::types::ReadinessCheck::LogContains { text } if text == "READY"));
    }

    /// 予約語・controller と同じ id・存在しないバリエーション・Core の名前空間の上書き・
    /// 未登録の依存は、解決の前に拒否する。
    #[test]
    fn invalid_projects_are_rejected() {
        let (assets, project) = split_v1_workspace(&v1_demo());
        let library = library_of(assets);
        let broken = |edit: &dyn Fn(&mut Project)| {
            let mut copy = project.clone();
            edit(&mut copy);
            resolve(&copy, &library).unwrap_err()
        };
        assert!(broken(&|p| p.members[0].id = CORE_SENTINEL.to_owned()).contains("予約語"));
        assert!(broken(&|p| {
            p.core_controller = Some(serde_json::from_value(json!({ "id": "pilot", "name": "ctl", "command": { "program": "x" } })).unwrap());
        })
        .contains("コントローラー"));
        assert!(broken(&|p| p.members[0].variant_id = "missing".to_owned()).contains("バリエーション"));
        assert!(broken(&|p| p.members[0].asset_id = "missing".to_owned()).contains("ライブラリ"));
        assert!(broken(&|p| {
            p.members[0].env_overrides.insert("HAKO_CONFIG_PATH".to_owned(), "D:/other.json".to_owned());
        })
        .contains("HAKO_CONFIG_PATH"));
        assert!(broken(&|p| p.members[1].depends_on.push("ghost".to_owned())).contains("ghost"));
        assert!(broken(&|p| {
            let id = p.members[1].id.clone();
            p.members[1].optional_depends_on.push(id);
        })
        .contains("自分自身"));
        assert!(broken(&|p| p.members[1].id = p.members[0].id.clone()).contains("重複"));
        assert_eq!(project.members[0].variant_id, DEFAULT_VARIANT);
    }

    /// v1 から作ったライブラリの id は、同じ v1 から作り直すと同じになる（移行の冪等性の前提）。
    #[test]
    fn v1_split_ids_are_deterministic() {
        let (first, _) = split_v1_workspace(&v1_demo());
        let (second, _) = split_v1_workspace(&v1_demo());
        assert_eq!(first.iter().map(|a| &a.id).collect::<Vec<_>>(), second.iter().map(|a| &a.id).collect::<Vec<_>>());
        let ids: std::collections::BTreeSet<_> = first.iter().map(|a| &a.id).collect();
        assert_eq!(ids.len(), first.len());
    }
}
