//! 接続（ConnectionDefinition）と登録アセット（AssetDefinition）の対応付け。
//!
//! importerが接続に入れる`source`/`destination`はエンドポイント名・URI・固定文字列で、
//! アセットの識別子（UUID）とは別物である。両者を突き合わせる規則をここに一本化し、
//! 結果を`*_asset_id`として持たせる。表示側は突き合わせを行わず、結果を読むだけにする。
//!
//! 解決結果は永続化されるが、それはキャッシュであって真実ではない。アセットの追加・
//! 改名・削除に追従させるため、スナップショット生成のたびに再計算する。

use crate::types::{AssetDefinition, ConnectionDefinition};
use std::collections::BTreeMap;

/// `source`/`destination`に入りうる、相手がアセットでないことを示す既定値。
/// importer自身が置くフォールバック文字列なので、アセット名として照合せず、
/// 解決できなくても警告の対象にしない。
const NON_ASSET_ENDPOINTS: [&str; 5] = [
    "external endpoint",
    "configured peer",
    "internal cache",
    "source endpoint",
    "destination endpoint",
];

/// 接続の`source`/`destination`/所有者を登録アセットへ解決し、`*_asset_id`を埋める。
/// 解決できなかった接続についての警告文を返す。
///
/// 以前の解決結果には依存せず毎回すべて計算し直すため、アセットが消えれば
/// 対応する`*_asset_id`もNoneへ戻る。
pub fn resolve_links(assets: &[AssetDefinition], connections: &mut [ConnectionDefinition]) -> Vec<String> {
    let index = AssetIndex::build(assets);
    let mut warnings = Vec::new();
    for connection in connections.iter_mut() {
        let source_asset_id = index.resolve_endpoint(&connection.source);
        let destination_asset_id = index.resolve_endpoint(&connection.destination);
        let owner_asset_id = index
            .resolve_owner(connection)
            .or_else(|| source_asset_id.clone());

        // 警告は端点ごとに出す。まとめて「何も紐づかない」とだけ言うと、
        // どちらの名前を直せばよいのかが利用者に伝わらない。
        for (role, value, resolved) in [
            ("送信元", &connection.source, &source_asset_id),
            ("宛先", &connection.destination, &destination_asset_id),
        ] {
            if resolved.is_none() && !looks_non_asset(value) {
                warnings.push(format!(
                    "接続「{}」の{}「{}」を登録アセットに紐づけられませんでした。Launcherのasset名を合わせるか、assetのコマンド引数に接続設定ファイルのパスを含めてください。",
                    connection.label, role, value.trim()
                ));
            }
        }

        connection.source_asset_id = source_asset_id;
        connection.destination_asset_id = destination_asset_id;
        connection.owner_asset_id = owner_asset_id;
    }
    warnings
}

/// 相手がアセットではないことを示す固定値かどうか。
pub fn looks_non_asset(value: &str) -> bool {
    let candidate = value.trim();
    NON_ASSET_ENDPOINTS.iter().any(|known| known.eq_ignore_ascii_case(candidate))
}

/// 表記ゆれを吸収した比較用のキー。区切り文字と大小差を無視する。
/// `parse_launcher`はasset名をtrimするが`parse_endpoint`はtrimしないため、
/// 素の文字列比較では同じ相手を取りこぼす。
fn normalize_name(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !matches!(character, '-' | '_' | ' ' | '.'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// パスから照合キーを作る。区切り文字はOS差があるため両方で切る。
fn path_tokens(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let file_name = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    let stem = file_name.rsplit_once('.').map(|(head, _)| head).unwrap_or(file_name);
    let mut tokens = vec![trimmed.to_lowercase(), file_name.to_lowercase()];
    if !stem.is_empty() {
        tokens.push(stem.to_lowercase());
        tokens.push(normalize_name(stem));
    }
    tokens.retain(|token| !token.is_empty());
    tokens.sort();
    tokens.dedup();
    tokens
}

struct AssetIndex {
    by_exact_name: BTreeMap<String, String>,
    by_normalized_name: BTreeMap<String, String>,
    /// 設定ファイルのパス断片 → アセットID。複数アセットが同じ断片を持つ場合は
    /// どちらとも決められないので、キーごと捨てて誤結合を避ける。
    by_config_token: BTreeMap<String, String>,
}

impl AssetIndex {
    fn build(assets: &[AssetDefinition]) -> Self {
        let mut by_exact_name = BTreeMap::new();
        let mut by_normalized_name = BTreeMap::new();
        let mut config_candidates: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for asset in assets {
            by_exact_name.entry(asset.name.trim().to_owned()).or_insert_with(|| asset.id.clone());
            let normalized = normalize_name(&asset.name);
            if !normalized.is_empty() {
                by_normalized_name.entry(normalized).or_insert_with(|| asset.id.clone());
            }
            // アセットが受け取る設定ファイルは、名前規約に依存しない突合材料になる。
            let sources = asset
                .config_files
                .iter()
                .chain(asset.command.args.iter())
                .chain(asset.command.cwd.iter());
            for value in sources {
                for token in path_tokens(value) {
                    config_candidates.entry(token).or_default().push(asset.id.clone());
                }
            }
        }

        let by_config_token = config_candidates
            .into_iter()
            .filter_map(|(token, owners)| {
                let first = owners.first()?.clone();
                owners.iter().all(|owner| owner == &first).then_some((token, first))
            })
            .collect();

        Self { by_exact_name, by_normalized_name, by_config_token }
    }

    fn resolve_endpoint(&self, value: &str) -> Option<String> {
        let needle = value.trim();
        if needle.is_empty() || looks_non_asset(needle) {
            return None;
        }
        if let Some(id) = self.by_exact_name.get(needle) {
            return Some(id.clone());
        }
        let normalized = normalize_name(needle);
        if let Some(id) = self.by_normalized_name.get(&normalized) {
            return Some(id.clone());
        }
        // エンドポイント名が設定ファイル名から採られている場合を拾う。
        path_tokens(needle).iter().find_map(|token| self.by_config_token.get(token).cloned())
    }

    /// この接続を観測しているアセット。Bridge由来では端点ではなくBridge自身。
    fn resolve_owner(&self, connection: &ConnectionDefinition) -> Option<String> {
        if let Some(bridge) = connection.details.get("bridge") {
            if let Some(id) = self
                .by_exact_name
                .get(bridge.trim())
                .or_else(|| self.by_normalized_name.get(&normalize_name(bridge)))
            {
                return Some(id.clone());
            }
        }
        let config_paths = connection
            .endpoint_config
            .iter()
            .chain(connection.details.get("comm_config"))
            .chain(connection.details.get("bridge_config"))
            .chain(connection.details.get("endpoint_config"));
        for path in config_paths {
            if let Some(id) = path_tokens(path).iter().find_map(|token| self.by_config_token.get(token).cloned()) {
                return Some(id);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ActivationTiming, AssetRole, ExecutionTarget, ProgramSpec, TransportKind};

    fn asset(name: &str, args: Vec<&str>) -> AssetDefinition {
        AssetDefinition {
            id: format!("id-of-{name}"),
            name: name.to_owned(),
            role: AssetRole::Other,
            command: ProgramSpec {
                program: "python".to_owned(),
                args: args.into_iter().map(str::to_owned).collect(),
                cwd: None,
                env: BTreeMap::new(),
                target: ExecutionTarget::Native,
            },
            depends_on: Vec::new(),
            activation_timing: ActivationTiming::Manual,
            config_files: Vec::new(),
            enabled: true,
        }
    }

    fn connection(source: &str, destination: &str) -> ConnectionDefinition {
        ConnectionDefinition {
            id: "conn".to_owned(),
            source: source.to_owned(),
            destination: destination.to_owned(),
            label: "Endpoint: test".to_owned(),
            transport: TransportKind::SharedMemory,
            pdu_names: Vec::new(),
            endpoint_config: None,
            details: BTreeMap::new(),
            source_asset_id: None,
            destination_asset_id: None,
            owner_asset_id: None,
        }
    }

    /// 接続はアセット名で書かれるがアセットの識別子はUUIDなので、
    /// 解決を通さないと表示側は両者を突き合わせられない。
    #[test]
    fn links_endpoints_to_assets_by_name() {
        let assets = vec![asset("pdu-bridge-sim", vec![]), asset("twist-consumer", vec![])];
        let mut connections = vec![connection("pdu-bridge-sim", "twist-consumer")];
        let warnings = resolve_links(&assets, &mut connections);
        assert_eq!(connections[0].source_asset_id.as_deref(), Some("id-of-pdu-bridge-sim"));
        assert_eq!(connections[0].destination_asset_id.as_deref(), Some("id-of-twist-consumer"));
        assert!(warnings.is_empty(), "解決できているのに警告が出ています: {warnings:?}");
    }

    /// parse_launcherはnameをtrimするがparse_endpointはtrimしない。
    /// 区切り文字や大小の違いも実データでは普通に起きる。
    #[test]
    fn links_endpoints_despite_whitespace_and_case() {
        let assets = vec![asset("PDU Bridge Sim", vec![])];
        let mut connections = vec![connection("  pdu-bridge-sim  ", "external endpoint")];
        let warnings = resolve_links(&assets, &mut connections);
        assert_eq!(connections[0].source_asset_id.as_deref(), Some("id-of-PDU Bridge Sim"));
        assert!(warnings.is_empty(), "表記ゆれの吸収に失敗しています: {warnings:?}");
    }

    /// 名前がまったく一致しなくても、アセットが受け取る設定ファイルのパスで結び付く。
    /// 名前規約に依存しない唯一の経路なので、これが落ちると実データで機能しない。
    ///
    /// 端点の名前は設定ファイル名とも無関係にして、パス経由の解決だけが効く形にする。
    #[test]
    fn links_owner_by_config_path_in_command_args() {
        let assets = vec![asset("launcher-managed-asset", vec!["--config", "/recipe/endpoint_a.json"])];
        let mut connections = vec![connection("shm-channel-7", "external endpoint")];
        connections[0].endpoint_config = Some("/recipe/endpoint_a.json".to_owned());
        resolve_links(&assets, &mut connections);
        assert_eq!(
            connections[0].owner_asset_id.as_deref(),
            Some("id-of-launcher-managed-asset"),
            "設定ファイルのパス経由で所有者を解決できていません。"
        );
    }

    /// エンドポイント名が設定ファイル名から採られている場合も拾えること。
    #[test]
    fn links_endpoint_named_after_its_config_file() {
        let assets = vec![asset("launcher-managed-asset", vec!["--config", "/recipe/endpoint_a.json"])];
        let mut connections = vec![connection("endpoint_a", "external endpoint")];
        let warnings = resolve_links(&assets, &mut connections);
        assert_eq!(connections[0].source_asset_id.as_deref(), Some("id-of-launcher-managed-asset"));
        assert!(warnings.is_empty(), "解決できているのに警告が出ています: {warnings:?}");
    }

    /// Bridge由来の接続では、端点ではなくBridgeアセットが観測者になる。
    #[test]
    fn links_owner_by_bridge_name() {
        let assets = vec![asset("pdu-bridge", vec![])];
        let mut connections = vec![connection("source endpoint", "destination endpoint")];
        connections[0].details.insert("bridge".to_owned(), "pdu-bridge".to_owned());
        resolve_links(&assets, &mut connections);
        assert_eq!(connections[0].owner_asset_id.as_deref(), Some("id-of-pdu-bridge"));
        assert!(connections[0].source_asset_id.is_none());
    }

    /// 相手が外部ホストやキャッシュの接続はNoneのままが正しく、警告も出さない。
    /// 既定値を除外し損ねると、正常な構成に対して毎回警告が出て信用されなくなる。
    #[test]
    fn keeps_non_asset_endpoints_unlinked_and_silent() {
        let assets = vec![asset("pdu-bridge-sim", vec![])];
        let mut connections = vec![connection("pdu-bridge-sim", "internal cache")];
        let warnings = resolve_links(&assets, &mut connections);
        assert!(connections[0].destination_asset_id.is_none());
        assert!(warnings.is_empty(), "アセットでない相手に警告を出しています: {warnings:?}");
    }

    /// 紐づかない端点は黙って0件にせず、どちらの名前が問題かを名指しで知らせる。
    #[test]
    fn warns_for_each_unresolved_endpoint() {
        let assets = vec![asset("pdu-bridge-sim", vec![])];
        let mut connections = vec![connection("unknown-a", "unknown-b")];
        let warnings = resolve_links(&assets, &mut connections);
        assert!(connections[0].source_asset_id.is_none());
        assert!(connections[0].owner_asset_id.is_none());
        assert_eq!(warnings.len(), 2, "警告の件数が想定と違います: {warnings:?}");
        assert!(warnings.iter().any(|warning| warning.contains("unknown-a")));
        assert!(warnings.iter().any(|warning| warning.contains("unknown-b")));
    }

    /// 同じ設定ファイルを2つのアセットが参照している場合、どちらとも決められない。
    /// 先勝ちで誤った相手に結び付けるより、未解決のままにする。
    #[test]
    fn refuses_ambiguous_config_path_match() {
        let assets = vec![
            asset("asset-a", vec!["/recipe/shared.json"]),
            asset("asset-b", vec!["/recipe/shared.json"]),
        ];
        let mut connections = vec![connection("shared", "external endpoint")];
        connections[0].endpoint_config = Some("/recipe/shared.json".to_owned());
        resolve_links(&assets, &mut connections);
        assert!(connections[0].source_asset_id.is_none(), "曖昧な候補を勝手に選んでいます。");
        assert!(connections[0].owner_asset_id.is_none(), "曖昧な候補を勝手に選んでいます。");
    }

    /// アセットの追加・削除に追従すること。永続化された解決結果を信用しない根拠。
    #[test]
    fn re_resolution_follows_asset_changes() {
        let mut assets = vec![asset("pdu-bridge-sim", vec![])];
        let mut connections = vec![connection("pdu-bridge-sim", "twist-consumer")];
        resolve_links(&assets, &mut connections);
        assert!(connections[0].destination_asset_id.is_none());

        assets.push(asset("twist-consumer", vec![]));
        resolve_links(&assets, &mut connections);
        assert_eq!(connections[0].destination_asset_id.as_deref(), Some("id-of-twist-consumer"));

        assets.pop();
        resolve_links(&assets, &mut connections);
        assert!(connections[0].destination_asset_id.is_none(), "消えたアセットへの参照が残っています。");
    }
}
