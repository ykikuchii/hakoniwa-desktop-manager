use crate::types::{
    ActivationTiming, AssetDefinition, AssetRole, ConnectionDefinition,
    ExecutionTarget, ImportPreview, ProgramSpec, TransportKind,
};
use serde_json::Value;
use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Path, PathBuf}};
use uuid::Uuid;
use walkdir::WalkDir;

pub fn inspect_directory(directory: &Path) -> Result<ImportPreview, String> {
    if !directory.is_dir() {
        return Err("指定したディレクトリが見つかりません。".to_owned());
    }
    let mut discovered_files = Vec::new();
    let mut documents = Vec::new();
    let mut warnings = Vec::new();
    for entry in WalkDir::new(directory).follow_links(false).max_depth(8).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() || entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let relative = entry.path().strip_prefix(directory).unwrap_or(entry.path()).display().to_string();
        discovered_files.push(relative.clone());
        match fs::read_to_string(entry.path()).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok()) {
            Some(value) => documents.push((entry.path().to_path_buf(), value)),
            None => warnings.push(format!("JSONとして読み取れませんでした: {relative}")),
        }
    }
    discovered_files.sort();
    let mut assets = Vec::new();
    let mut connections = Vec::new();
    for (path, value) in &documents {
        let filename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        if filename.contains("launch") || value.get("assets").and_then(Value::as_array).is_some() {
            let mut parsed = parse_launcher(path, value, &mut warnings);
            assets.append(&mut parsed);
        }
        if filename.contains("endpoint") || looks_like_endpoint(value) {
            if let Some(connection) = parse_endpoint(path, value, directory, &documents, &mut warnings) {
                connections.push(connection);
            }
        }
        if filename.contains("bridge") || looks_like_bridge(value) {
            let mut parsed = parse_bridge(path, value, &mut warnings);
            connections.append(&mut parsed);
        }
    }
    deduplicate_assets(&mut assets);
    deduplicate_connections(&mut connections);
    warnings.extend(crate::linking::resolve_links(&assets, &mut connections));
    if assets.is_empty() {
        warnings.push("Launcher形式のassets[]を検出できませんでした。アセットは画面から個別に追加できます。".to_owned());
    }
    if connections.is_empty() {
        warnings.push("EndpointまたはBridgeの接続設定を検出できませんでした。通信状態は手動登録または対応アダプタで確認できます。".to_owned());
    }
    Ok(ImportPreview {
        source_directory: directory.display().to_string(),
        discovered_files,
        assets,
        connections,
        warnings,
    })
}

fn parse_launcher(path: &Path, root: &Value, warnings: &mut Vec<String>) -> Vec<AssetDefinition> {
    let Some(items) = root.get("assets").and_then(Value::as_array) else { return Vec::new(); };
    items.iter().filter_map(|item| {
        let name = item.get("name").and_then(Value::as_str)?.trim();
        let command = item.get("command").and_then(Value::as_str)?.trim();
        if name.is_empty() || command.is_empty() {
            warnings.push(format!("{}: assetのnameまたはcommandが不足しています。", path.display()));
            return None;
        }
        if command.split_whitespace().count() > 1 {
            warnings.push(format!("{}: asset「{}」のcommandに空白があります。安全のためコマンドと引数を画面で分けて確認してください。", path.display(), name));
        }
        let args = item.get("args").and_then(Value::as_array).map(|values| values.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default();
        let env = item.get("env").and_then(Value::as_object).map(|values| values.iter().filter_map(|(key, value)| value.as_str().map(|string| (key.clone(), string.to_owned()))).collect()).unwrap_or_default();
        let depends_on = item.get("depends_on").and_then(Value::as_array).map(|values| values.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default();
        let activation_timing = match item.get("activation_timing").and_then(Value::as_str) {
            Some("before_start") => ActivationTiming::BeforeStart,
            Some("after_start") => ActivationTiming::AfterStart,
            _ => ActivationTiming::Manual,
        };
        Some(AssetDefinition {
            id: stable_id(&format!("{}:{name}", path.display())),
            name: name.to_owned(),
            role: infer_role(name, command),
            command: ProgramSpec {
                program: command.to_owned(), args,
                cwd: item.get("cwd").and_then(Value::as_str).map(str::to_owned),
                env, target: ExecutionTarget::Native,
            },
            depends_on,
            activation_timing,
            config_files: vec![path.display().to_string()],
            enabled: true,
        })
    }).collect()
}

fn parse_endpoint(
    path: &Path,
    root: &Value,
    base_directory: &Path,
    documents: &[(PathBuf, Value)],
    warnings: &mut Vec<String>,
) -> Option<ConnectionDefinition> {
    let name = root.get("name").and_then(Value::as_str).or_else(|| path.file_stem().and_then(|name| name.to_str()))?;
    let mut details = BTreeMap::new();
    details.insert("endpoint_config".to_owned(), path.display().to_string());
    let mut transport = TransportKind::Unknown;
    let mut destination = "external endpoint".to_owned();
    if let Some(comm_path) = root.get("comm").and_then(Value::as_str) {
        let resolved = path.parent().unwrap_or(base_directory).join(comm_path).canonicalize().unwrap_or_else(|_| path.parent().unwrap_or(base_directory).join(comm_path));
        details.insert("comm_config".to_owned(), resolved.display().to_string());
        if let Some((_, comm)) = documents.iter().find(|(candidate, _)| candidate == &resolved) {
            let protocol = comm.get("protocol").or_else(|| comm.get("type")).and_then(Value::as_str).unwrap_or("unknown");
            transport = parse_transport(protocol);
            destination = comm.get("uri").or_else(|| comm.get("host")).or_else(|| comm.get("address")).and_then(Value::as_str).unwrap_or("configured peer").to_owned();
            details.insert("protocol".to_owned(), protocol.to_owned());
            if let Some(port) = comm.get("port").and_then(Value::as_u64) { details.insert("port".to_owned(), port.to_string()); }
            if let Some(role) = comm.get("role").and_then(Value::as_str) { details.insert("role".to_owned(), role.to_owned()); }
        } else {
            warnings.push(format!("{}: comm設定 {} を解析できませんでした。", path.display(), comm_path));
        }
    } else if root.get("comm").is_some() {
        transport = TransportKind::Storage;
        destination = "internal cache".to_owned();
    }
    let pdu_names = root.get("pdu_def_path").and_then(Value::as_str).map(|value| vec![value.to_owned()]).unwrap_or_default();
    Some(ConnectionDefinition {
        id: stable_id(&format!("endpoint:{}", path.display())),
        source: name.to_owned(),
        destination,
        label: format!("Endpoint: {name}"),
        transport,
        pdu_names,
        endpoint_config: Some(path.display().to_string()),
        details,
        source_asset_id: None,
        destination_asset_id: None,
        owner_asset_id: None,
    })
}

fn parse_bridge(path: &Path, root: &Value, warnings: &mut Vec<String>) -> Vec<ConnectionDefinition> {
    let mut result = Vec::new();
    let bridge_name = root.get("name").and_then(Value::as_str).unwrap_or_else(|| path.file_stem().and_then(|name| name.to_str()).unwrap_or("bridge"));
    let rules = root.get("routes").or_else(|| root.get("bridges")).or_else(|| root.get("mappings")).and_then(Value::as_array);
    if let Some(rules) = rules {
        for (index, rule) in rules.iter().enumerate() {
            let source = rule.get("source").or_else(|| rule.get("from")).and_then(Value::as_str).unwrap_or("source endpoint");
            let destination = rule.get("destination").or_else(|| rule.get("to")).and_then(Value::as_str).unwrap_or("destination endpoint");
            let mut details = BTreeMap::new();
            details.insert("bridge_config".to_owned(), path.display().to_string());
            details.insert("bridge".to_owned(), bridge_name.to_owned());
            if let Some(policy) = rule.get("policy").or_else(|| rule.get("transfer_policy")).and_then(Value::as_str) { details.insert("transfer_policy".to_owned(), policy.to_owned()); }
            let pdu_names = rule.get("pdus").or_else(|| rule.get("pdu_names")).and_then(Value::as_array).map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default();
            result.push(ConnectionDefinition { id: stable_id(&format!("bridge:{}:{index}", path.display())), source: source.to_owned(), destination: destination.to_owned(), label: format!("Bridge: {bridge_name}"), transport: TransportKind::Unknown, pdu_names, endpoint_config: Some(path.display().to_string()), details, source_asset_id: None, destination_asset_id: None, owner_asset_id: None });
        }
    } else {
        warnings.push(format!("{}: Bridgeらしい設定を検出しましたが、routes/bridges/mappingsの配列を認識できませんでした。", path.display()));
    }
    result
}

fn looks_like_endpoint(root: &Value) -> bool {
    root.get("cache").is_some() && root.get("comm").is_some()
}

fn looks_like_bridge(root: &Value) -> bool {
    root.get("routes").is_some() || root.get("bridges").is_some() || root.get("mappings").is_some()
}

fn parse_transport(value: &str) -> TransportKind {
    match value.to_lowercase().as_str() {
        "shm" | "shared_memory" | "shared-memory" => TransportKind::SharedMemory,
        "tcp" | "tcp_mux" => TransportKind::Tcp,
        "udp" => TransportKind::Udp,
        "websocket" | "ws" => TransportKind::Websocket,
        "zenoh" | "rmw_zenoh" => TransportKind::Zenoh,
        "mqtt" => TransportKind::Mqtt,
        "rpc" => TransportKind::Rpc,
        "storage" => TransportKind::Storage,
        _ => TransportKind::Unknown,
    }
}

fn infer_role(name: &str, command: &str) -> AssetRole {
    let text = format!("{} {}", name.to_lowercase(), command.to_lowercase());
    if text.contains("bridge") { AssetRole::Bridge }
    else if text.contains("viewer") || text.contains("visual") || text.contains("foxglove") { AssetRole::Visualizer }
    else if text.contains("controller") || text.contains("control") { AssetRole::Controller }
    else if text.contains("monitor") { AssetRole::Monitor }
    else if text.contains("sim") || text.contains("mujoco") || text.contains("drone") { AssetRole::Simulator }
    else { AssetRole::Other }
}

fn stable_id(seed: &str) -> String { Uuid::new_v5(&Uuid::NAMESPACE_URL, seed.as_bytes()).to_string() }

fn deduplicate_assets(assets: &mut Vec<AssetDefinition>) {
    let mut names = BTreeSet::new();
    assets.retain(|asset| names.insert(asset.id.clone()));
}

fn deduplicate_connections(connections: &mut Vec<ConnectionDefinition>) {
    let mut ids = BTreeSet::new();
    connections.retain(|connection| ids.insert(connection.id.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_transports() {
        assert_eq!(parse_transport("websocket"), TransportKind::Websocket);
        assert_eq!(parse_transport("shm"), TransportKind::SharedMemory);
    }

    #[test]
    fn parses_launcher_asset() {
        let root: Value = serde_json::json!({"assets":[{"name":"controller","command":"python","args":["controller.py"],"activation_timing":"after_start"}]});
        let assets = parse_launcher(Path::new("launch.json"), &root, &mut Vec::new());
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].activation_timing, ActivationTiming::AfterStart);
    }

    /// 突き合わせはlinking側の責務であり、parse層は解決結果を埋めない。
    #[test]
    fn parse_endpoint_leaves_links_unset() {
        let root: Value = serde_json::json!({"name":"endpoint_a","cache":{},"comm":{}});
        let connection = parse_endpoint(Path::new("endpoint_a.json"), &root, Path::new("."), &[], &mut Vec::new())
            .expect("endpointを解析できませんでした。");
        assert!(connection.source_asset_id.is_none());
        assert!(connection.destination_asset_id.is_none());
        assert!(connection.owner_asset_id.is_none());
    }
}
