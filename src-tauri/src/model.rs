//! 保存の正本になる v2 の型。ライブラリ（手元のアセット）とプロジェクト（組み合わせ）。
//!
//! 実行層と画面は従来の `Workspace` / `AssetDefinition` を使い続ける。プロジェクトは
//! `resolve::resolve` でライブラリと合わせて `Workspace` に解決してから渡す。


use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ports::{Access, FileDigest, Usage};
use crate::types::{ActivationTiming, AssetDefinition, AssetRole, ConnectionDefinition, CoreController, InstalledCoreSelection, ProgramSpec, ReadinessCheck, Workspace};

pub const STORE_SCHEMA_VERSION: u32 = 2;

/// 依存の番兵。Core（`hako-cmd start`）を表し、メンバー id には使えない。
pub const CORE_SENTINEL: &str = "core";

/// v1 から移行したバリエーションの id。
pub const DEFAULT_VARIANT: &str = "default";

/// メンバーの起動環境で上書きしてはいけない変数。Core の名前空間（共有メモリの
/// 場所）はプロジェクトの `core_env` だけで決める。
pub const CORE_NAMESPACE_VARIABLE: &str = "HAKO_CONFIG_PATH";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryAsset {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub role: AssetRole,
    #[serde(default)]
    pub description: String,
    /// 相対パスの基準。v1 から移行したものは絶対パスをそのまま持つので `None`。
    #[serde(default)]
    pub root_dir: Option<String>,
    pub variants: Vec<Variant>,
    pub updated_at: DateTime<Utc>,
}

/// アセットの起動のしかた 1 通り。引数で読む設定が変わるもの（`--legs full|rig` など）は
/// 別のバリエーションにする。それぞれが、それだけで完結した起動定義と PDU の契約を持つ。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variant {
    pub id: String,
    pub label: String,
    pub launch: ProgramSpec,
    /// 既定値。メンバーが上書きできる。
    #[serde(default)]
    pub activation_timing: ActivationTiming,
    /// 既定値。メンバーが上書きできる。
    #[serde(default)]
    pub readiness: ReadinessCheck,
    /// v1 の `config_files`。接続の推定や監視が使う。PDU 以外の設定も含む。
    #[serde(default)]
    pub config_files: Vec<String>,
    /// PDU 設定の入口。
    #[serde(default)]
    pub pdu_entries: Vec<PduEntrySpec>,
    /// ほかに変更を監視したいファイル（サイズを直接書いたスクリプトなど）。
    #[serde(default)]
    pub extra_inputs: Vec<String>,
    /// compact 形式で向きが分からないチャネルの、読み・書きの補足。
    #[serde(default)]
    pub manual_access: Vec<ManualAccess>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PduEntrySpec {
    pub path: String,
    pub usage: Usage,
    /// 取り込み時に推定したもので、利用者が確かめていない。
    #[serde(default)]
    pub usage_inferred: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManualAccess {
    pub robot: String,
    pub channel_id: i64,
    pub access: Access,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    /// どこから来たか。同じ出所を取り込み直したとき、増やさずに置き換える相手を探すのに使う。
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub source_directory: Option<String>,
    #[serde(default)]
    pub core_release: Option<InstalledCoreSelection>,
    #[serde(default)]
    pub core_controller: Option<CoreController>,
    #[serde(default)]
    pub core_env: BTreeMap<String, String>,
    /// 順序に意味がある（一括停止は逆順に止める）。
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default)]
    pub connections: Vec<ConnectionDefinition>,
    #[serde(default)]
    pub last_opened_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    pub asset_id: String,
    pub variant_id: String,
    /// 表示名。同じアセットを 2 回使うときなどに付ける。なければアセットの名前。
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
    /// 利用者が承認した時点の内容。一括起動の前の照合に使う。
    #[serde(default)]
    pub accepted: Option<Acceptance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Acceptance {
    pub revision: String,
    pub manifest: Vec<FileDigest>,
    pub accepted_at: DateTime<Utc>,
    /// 承認したときに PDU 設定の入口があったか。後で入口がなくなったことを、
    /// manifest（ほかの監視ファイルも含む）から推し量らずに判断するため。
    #[serde(default)]
    pub had_pdu_entries: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub active_project_id: Option<String>,
    #[serde(default)]
    pub migrated_from_v1: Option<MigrationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationRecord {
    pub source_sha256: String,
    pub at: DateTime<Utc>,
}

/// ライブラリ。id からアセットを引く。
pub type Library = BTreeMap<String, LibraryAsset>;

impl LibraryAsset {
    pub fn variant(&self, id: &str) -> Option<&Variant> {
        self.variants.iter().find(|variant| variant.id == id)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != STORE_SCHEMA_VERSION {
            return Err(format!("アセット「{}」の形式（v{}）は未対応です。", self.name, self.schema_version));
        }
        if self.id.trim().is_empty() {
            return Err("アセット ID が空です。".to_owned());
        }
        if self.name.trim().is_empty() {
            return Err("アセット名を指定してください。".to_owned());
        }
        if self.variants.is_empty() {
            return Err(format!("アセット「{}」に起動のしかた（バリエーション）がありません。", self.name));
        }
        let mut ids = BTreeSet::new();
        for variant in &self.variants {
            if !ids.insert(variant.id.as_str()) {
                return Err(format!("アセット「{}」のバリエーション ID「{}」が重複しています。", self.name, variant.id));
            }
            variant.launch.validate()?;
        }
        Ok(())
    }
}

impl Project {
    pub fn empty(id: String, name: String) -> Self {
        Self {
            schema_version: STORE_SCHEMA_VERSION,
            id,
            name,
            origin: None,
            source_directory: None,
            core_release: None,
            core_controller: None,
            core_env: BTreeMap::new(),
            members: Vec::new(),
            connections: Vec::new(),
            last_opened_at: Some(Utc::now()),
        }
    }

    pub fn member(&self, id: &str) -> Option<&Member> {
        self.members.iter().find(|member| member.id == id)
    }

    pub fn validate(&self, library: &Library) -> Result<(), String> {
        if self.schema_version != STORE_SCHEMA_VERSION {
            return Err(format!("プロジェクト「{}」の形式（v{}）は未対応です。", self.name, self.schema_version));
        }
        if self.id.trim().is_empty() {
            return Err("プロジェクト ID が空です。".to_owned());
        }
        if self.name.trim().is_empty() {
            return Err("プロジェクト名を指定してください。".to_owned());
        }
        let controller_id = self.core_controller.as_ref().map(|controller| controller.id.as_str());
        let mut ids = BTreeSet::new();
        for member in &self.members {
            let label = member.name.as_deref().unwrap_or(&member.id);
            if member.id.trim().is_empty() {
                return Err("メンバー ID が空です。".to_owned());
            }
            if member.id == CORE_SENTINEL {
                return Err(format!("メンバー ID に「{CORE_SENTINEL}」は使えません（Core を表す予約語です）。"));
            }
            if Some(member.id.as_str()) == controller_id {
                return Err(format!("メンバー「{label}」の ID が Core コントローラーと同じです。"));
            }
            if !ids.insert(member.id.as_str()) {
                return Err(format!("メンバー ID「{}」が重複しています。", member.id));
            }
            let asset = library.get(&member.asset_id).ok_or_else(|| format!("メンバー「{label}」のアセット（{}）がライブラリにありません。", member.asset_id))?;
            if asset.variant(&member.variant_id).is_none() {
                return Err(format!("メンバー「{label}」のバリエーション「{}」が、アセット「{}」にありません。", member.variant_id, asset.name));
            }
            if member.env_overrides.contains_key(CORE_NAMESPACE_VARIABLE) {
                return Err(format!("メンバー「{label}」で {CORE_NAMESPACE_VARIABLE} は上書きできません。Core の設定はプロジェクトの Core 環境で指定してください。"));
            }
        }
        for member in &self.members {
            let label = member.name.as_deref().unwrap_or(&member.id);
            for dependency in member.depends_on.iter().chain(member.optional_depends_on.iter()) {
                if dependency == &member.id {
                    return Err(format!("メンバー「{label}」が自分自身に依存しています。"));
                }
                if dependency != CORE_SENTINEL && !ids.contains(dependency.as_str()) {
                    return Err(format!("メンバー「{label}」は、プロジェクトにない依存先「{dependency}」を参照しています。"));
                }
            }
        }
        Ok(())
    }
}

impl AppConfig {
    pub fn new() -> Self {
        Self { schema_version: STORE_SCHEMA_VERSION, active_project_id: None, migrated_from_v1: None }
    }
}

/// v1 のワークスペースから作るライブラリのアセットの id。出所（[`v1_origin`]）と
/// v1 のアセット id で決まる。生成スクリプトはワークスペース id を毎回新しく振るので、
/// それを使うと取り込み直すたびにライブラリが増える。
pub fn v1_library_asset_id(origin: &str, asset_id: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("hakoniwa-desktop-manager:v1-asset:{origin}:{asset_id}").as_bytes()).to_string()
}

/// v1 のワークスペースの出所。`source_directory` と名前で、同じ生成元かを見分ける。
pub fn v1_origin(workspace: &Workspace) -> String {
    format!("v1:{}|{}", workspace.source_directory.as_deref().unwrap_or_default(), workspace.name)
}

/// v1 のアセット定義 1 つから、バリエーション 1 つのライブラリのアセットを作る。
pub fn library_asset_from_definition(id: String, asset: &AssetDefinition) -> LibraryAsset {
    LibraryAsset {
        schema_version: STORE_SCHEMA_VERSION,
        id,
        name: asset.name.clone(),
        role: asset.role.clone(),
        description: String::new(),
        root_dir: None,
        variants: vec![Variant {
            id: DEFAULT_VARIANT.to_owned(),
            label: "既定".to_owned(),
            launch: asset.command.clone(),
            activation_timing: asset.activation_timing.clone(),
            readiness: asset.readiness.clone(),
            config_files: asset.config_files.clone(),
            pdu_entries: Vec::new(),
            extra_inputs: Vec::new(),
            manual_access: Vec::new(),
        }],
        updated_at: Utc::now(),
    }
}

/// v1 のアセット定義 1 つから、プロジェクトのメンバーを作る。起動タイミングと準備完了の
/// 条件はバリエーションの既定値に入れたので、メンバー側では上書きしない。
pub fn member_from_definition(asset: &AssetDefinition, library_asset_id: String) -> Member {
    Member {
        id: asset.id.clone(),
        asset_id: library_asset_id,
        variant_id: DEFAULT_VARIANT.to_owned(),
        name: None,
        enabled: asset.enabled,
        depends_on: asset.depends_on.clone(),
        optional_depends_on: asset.optional_depends_on.clone(),
        activation_timing: None,
        readiness: None,
        env_overrides: BTreeMap::new(),
        accepted: None,
    }
}

/// v1 のワークスペースを、ライブラリのアセット群とプロジェクトに分ける。
///
/// プロジェクトの id は v1 のワークスペースの id を、メンバー id は v1 のアセット id を
/// 引き継ぐ。解決すると元のワークスペースと同じものに戻る。
pub fn split_v1_workspace(workspace: &Workspace) -> (Vec<LibraryAsset>, Project) {
    let origin = v1_origin(workspace);
    let mut assets = Vec::new();
    let mut members = Vec::new();
    for definition in &workspace.assets {
        let id = v1_library_asset_id(&origin, &definition.id);
        assets.push(library_asset_from_definition(id.clone(), definition));
        members.push(member_from_definition(definition, id));
    }
    let project = Project {
        schema_version: STORE_SCHEMA_VERSION,
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        origin: Some(origin),
        source_directory: workspace.source_directory.clone(),
        core_release: workspace.core_release.clone(),
        core_controller: workspace.core_controller.clone(),
        core_env: workspace.core_env.clone(),
        members,
        connections: workspace.imported_connections.clone(),
        last_opened_at: workspace.last_opened_at,
    };
    (assets, project)
}
