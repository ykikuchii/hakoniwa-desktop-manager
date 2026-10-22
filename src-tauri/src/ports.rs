//! アセットの PDU 設定ファイルから、そのアセットの「ポート」を抽出する。
//!
//! ポートは 2 種類に分けて持つ。
//!
//! - `registrations`: `asset_register` に渡す設定が作るチャネル。Core は reader も含めて
//!   宣言されたチャネルをすべて作る（hakoniwa-core-pro `hako_asset_impl.cpp`）。
//! - `access`: そのアセットが読む・書くと宣言しているチャネル。compact 形式
//!   （`custom.json` + `pdutypes.json`）は向きを持たないので `Unknown` になる。
//!
//! 形式の判定は Core と同じで、`paths` キーがあれば compact、`robots` があれば legacy
//! （`robots[].shm_pdu_readers` など）。どちらも持たず `pdu_config_path` や `services` を
//! 持つものは service 設定として扱う。service 設定の `pdu_config_path` はたどらない。
//! Core はこの値を読まず、Python の `ShmCommon` はプロセスの作業ディレクトリ基準で
//! 開くので、ファイルの場所からは解決できない。アセットが `PduManager` に渡す
//! PDU 設定は、入口として明示してもらう。読んだファイルはすべて SHA-256 を記録し、
//! 設定の変更を検知できるようにする。

// 呼び出し側（プロジェクトの resolve と一括起動前の確認）は次の段階で入る。
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// PDU 設定ファイルの使い方。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Usage {
    /// `asset_register` に渡し、宣言したチャネルを作る（所有する）。
    Register,
    /// `init_for_external` の後、既にあるチャネルにつなぐ。
    Attach,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    Write,
    /// compact 形式のように、向きを宣言していない。
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Shm,
    Rpc,
}

/// どこまで検証できたか。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// 宣言をすべて読めた。
    Complete,
    /// 一部に未対応の宣言（RPC の service 定義など）がある。
    Partial,
}

/// PDU 設定の入口のファイルと、その使い方。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PduEntry {
    pub path: PathBuf,
    pub usage: Usage,
}

/// 1 つのチャネルの宣言。チャネルは `(robot, channel_id)` で一意に決まる。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelDecl {
    pub robot: String,
    pub channel_id: i64,
    pub org_name: String,
    pub type_name: String,
    pub pdu_size: i64,
    pub transport: Transport,
    /// 宣言していたファイル。
    pub source: String,
}

impl ChannelDecl {
    pub fn key(&self) -> (String, i64) {
        (self.robot.clone(), self.channel_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessDecl {
    pub channel: ChannelDecl,
    pub access: Access,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileDigest {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortSet {
    pub registrations: Vec<ChannelDecl>,
    pub access: Vec<AccessDecl>,
    pub coverage: Coverage,
    /// 読めなかったファイルや、形式を解釈できなかった理由。1 件でもあれば、
    /// このポートは信用できない（空のポートとして通してはいけない）。
    pub problems: Vec<String>,
    /// 読んだファイル（入口と、そこから参照されたもの）。
    pub files: Vec<FileDigest>,
}

/// 入口のファイル群から、ポートを抽出する。
pub fn extract(entries: &[PduEntry]) -> PortSet {
    let mut extractor = Extractor::default();
    for entry in entries {
        extractor.entry(&entry.path, entry.usage);
    }
    extractor.finish()
}

#[derive(Default)]
struct Extractor {
    registrations: Vec<ChannelDecl>,
    access: Vec<AccessDecl>,
    partial: bool,
    problems: Vec<String>,
    files: Vec<FileDigest>,
    seen: BTreeSet<PathBuf>,
}

impl Extractor {
    fn finish(mut self) -> PortSet {
        self.files.sort_by(|a, b| a.path.cmp(&b.path));
        PortSet {
            registrations: self.registrations,
            access: self.access,
            coverage: if self.partial { Coverage::Partial } else { Coverage::Complete },
            problems: self.problems,
            files: self.files,
        }
    }

    /// ファイルを読んで JSON として返す。同じファイルは 1 回だけ記録する。
    ///
    /// Python の読み込み（`PduChannelConfig` は `encoding='utf-8'` で開く）は先頭の BOM を
    /// 受け付けない。C++ の Core は受け付けるので、BOM は `Attach` のときだけ問題にする。
    fn load(&mut self, path: &Path, usage: Usage) -> Option<Value> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.problems.push(format!("{} を読めません: {error}", path.display()));
                return None;
            }
        };
        if self.seen.insert(path.to_path_buf()) {
            self.files.push(FileDigest { path: path.display().to_string(), sha256: hex::encode(Sha256::digest(&bytes)) });
        }
        let bom = bytes.starts_with(b"\xef\xbb\xbf");
        if bom && usage == Usage::Attach {
            self.problems.push(format!("{} は先頭に BOM があり、Python の PDU 読み込みが失敗します", path.display()));
            return None;
        }
        let text = String::from_utf8_lossy(&bytes);
        match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
            Ok(value) => Some(value),
            Err(error) => {
                self.problems.push(format!("{} は JSON として読めません: {error}", path.display()));
                None
            }
        }
    }

    fn entry(&mut self, path: &Path, usage: Usage) {
        let Some(root) = self.load(path, usage) else { return };
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        if root.get("paths").is_some() {
            self.compact(path, base, &root, usage);
        } else if root.get("robots").is_some() {
            self.legacy(path, &root, usage);
        } else if root.get("pdu_config_path").is_some() || root.get("services").is_some() {
            self.service(&root);
        } else {
            self.problems.push(format!("{} は PDU 設定の形式ではありません（paths・robots・pdu_config_path のいずれもない）", path.display()));
        }
    }

    /// service 設定。ポートは持たず、変更の検知のために記録だけする。
    /// `services[]` や `nodes[]` の RPC 定義は検証の対象外なので、カバレッジを下げる。
    fn service(&mut self, root: &Value) {
        let non_empty = |key: &str| root.get(key).and_then(Value::as_array).is_some_and(|items| !items.is_empty());
        if non_empty("services") || non_empty("nodes") {
            self.partial = true;
        }
    }

    /// compact 形式。`paths[]` が pdutypes ファイルを、`robots[]` がそれを使う robot を示す。
    fn compact(&mut self, path: &Path, base: &Path, root: &Value, usage: Usage) {
        let source = path.display().to_string();
        let Some(paths) = root.get("paths").and_then(Value::as_array) else {
            self.problems.push(format!("{source} の paths が配列ではありません"));
            return;
        };
        // Core も Python も、robot が使うかによらず paths[] のファイルをすべて読む。
        // 使われないファイルが壊れていても初期化に失敗するので、ここでも全部読む。
        let mut types: Vec<(String, PathBuf, Option<Value>)> = Vec::new();
        for item in paths {
            match (item.get("id").and_then(Value::as_str), item.get("path").and_then(Value::as_str)) {
                (Some(id), Some(file)) => {
                    // 同じ id が重なると、Core は最初を、Python は最後を使う。どちらとも決められない。
                    if types.iter().any(|(other, _, _)| other == id) {
                        self.problems.push(format!("{source} の paths に id {id} が重複しています"));
                        continue;
                    }
                    let types_path = base.join(file);
                    let loaded = self.load(&types_path, usage);
                    types.push((id.to_owned(), types_path, loaded));
                }
                _ => self.problems.push(format!("{source} の paths に id か path がない要素があります")),
            }
        }
        let Some(robots) = root.get("robots").and_then(Value::as_array) else {
            self.problems.push(format!("{source} の robots が配列ではありません"));
            return;
        };
        for robot in robots {
            let (Some(name), Some(types_id)) = (robot.get("name").and_then(Value::as_str), robot.get("pdutypes_id").and_then(Value::as_str)) else {
                self.problems.push(format!("{source} の robots に name か pdutypes_id がない要素があります"));
                continue;
            };
            let Some((_, types_path, loaded)) = types.iter().find(|(id, _, _)| id == types_id) else {
                self.problems.push(format!("{source} の robot {name} が参照する pdutypes_id {types_id} が paths にありません"));
                continue;
            };
            // 読めなかったファイルの問題は、読んだときに記録済み。
            let Some(channels) = loaded.clone() else { continue };
            let types_path = types_path.clone();
            let Some(channels) = channels.as_array() else {
                self.problems.push(format!("{} は pdutypes の配列ではありません", types_path.display()));
                continue;
            };
            let types_source = types_path.display().to_string();
            for channel in channels {
                let Some(decl) = self.channel(name, channel, "name", Transport::Shm, &types_source) else { continue };
                if usage == Usage::Register {
                    self.registrations.push(decl.clone());
                }
                self.access.push(AccessDecl { channel: decl, access: Access::Unknown });
            }
        }
    }

    /// legacy 形式。`robots[]` ごとに readers・writers を持つ。
    fn legacy(&mut self, path: &Path, root: &Value, usage: Usage) {
        let source = path.display().to_string();
        let Some(robots) = root.get("robots").and_then(Value::as_array) else {
            self.problems.push(format!("{source} の robots が配列ではありません"));
            return;
        };
        const LISTS: [(&str, Transport, Access); 4] = [
            ("shm_pdu_readers", Transport::Shm, Access::Read),
            ("shm_pdu_writers", Transport::Shm, Access::Write),
            ("rpc_pdu_readers", Transport::Rpc, Access::Read),
            ("rpc_pdu_writers", Transport::Rpc, Access::Write),
        ];
        for robot in robots {
            let Some(name) = robot.get("name").and_then(Value::as_str) else {
                self.problems.push(format!("{source} の robots に name がない要素があります"));
                continue;
            };
            for (list, transport, access) in LISTS {
                let items = match robot.get(list) {
                    None => continue,
                    Some(Value::Array(items)) => items,
                    Some(_) => {
                        self.problems.push(format!("{source} の robot {name} の {list} が配列ではありません"));
                        continue;
                    }
                };
                for item in items {
                    let Some(decl) = self.channel(name, item, "org_name", transport, &source) else { continue };
                    if usage == Usage::Register {
                        self.registrations.push(decl.clone());
                    }
                    self.access.push(AccessDecl { channel: decl, access });
                }
            }
        }
    }

    fn channel(&mut self, robot: &str, item: &Value, name_key: &str, transport: Transport, source: &str) -> Option<ChannelDecl> {
        let text = |key: &str| item.get(key).and_then(Value::as_str).map(str::to_owned);
        let number = |key: &str| item.get(key).and_then(Value::as_i64);
        match (number("channel_id"), number("pdu_size"), text(name_key), text("type")) {
            (Some(channel_id), Some(pdu_size), Some(org_name), Some(type_name)) => {
                Some(ChannelDecl { robot: robot.to_owned(), channel_id, org_name, type_name, pdu_size, transport, source: source.to_owned() })
            }
            _ => {
                self.problems.push(format!("{source} の robot {robot} に、channel_id・pdu_size・{name_key}・type のどれかがないチャネルがあります"));
                None
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// テストごとに空の一時ディレクトリを作る。
    pub(crate) fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hdm-ports-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn write(dir: &Path, name: &str, value: Value) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        path
    }

    pub(crate) fn compact_owner(dir: &Path, robot: &str, channels: &[(i64, &str, i64)]) -> PathBuf {
        let types: Vec<Value> = channels.iter().map(|(id, name, size)| json!({ "channel_id": id, "pdu_size": size, "name": name, "type": "geometry_msgs/Twist" })).collect();
        write(dir, &format!("{robot}-pdutypes.json"), Value::Array(types));
        write(dir, &format!("{robot}-custom.json"), json!({ "paths": [{ "id": "default", "path": format!("{robot}-pdutypes.json") }], "robots": [{ "name": robot, "pdutypes_id": "default" }] }))
    }

    pub(crate) fn legacy(dir: &Path, file: &str, robot: &str, readers: &[(i64, &str, i64)], writers: &[(i64, &str, i64)]) -> PathBuf {
        let list = |items: &[(i64, &str, i64)]| -> Vec<Value> {
            items.iter().map(|(id, name, size)| json!({ "type": "geometry_msgs/Twist", "org_name": name, "name": format!("{robot}_{name}"), "channel_id": id, "pdu_size": size, "method_type": "SHM" })).collect()
        };
        write(dir, file, json!({ "robots": [{ "name": robot, "rpc_pdu_readers": [], "rpc_pdu_writers": [], "shm_pdu_readers": list(readers), "shm_pdu_writers": list(writers) }] }))
    }

    /// compact は登録すれば全チャネルを所有し、向きは分からない。
    #[test]
    fn compact_register_owns_every_channel_with_unknown_access() {
        let dir = scratch("compact");
        let custom = compact_owner(&dir, "Rig", &[(0, "cmd", 136), (1, "attitude", 72)]);
        let ports = extract(&[PduEntry { path: custom, usage: Usage::Register }]);
        assert!(ports.problems.is_empty(), "{:?}", ports.problems);
        assert_eq!(ports.registrations.iter().map(|c| (c.channel_id, c.pdu_size)).collect::<Vec<_>>(), vec![(0, 136), (1, 72)]);
        assert!(ports.access.iter().all(|a| a.access == Access::Unknown));
        assert_eq!(ports.files.len(), 2, "custom.json と pdutypes.json の両方を記録する");
        assert_eq!(ports.coverage, Coverage::Complete);
    }

    /// legacy を attach で使うと、所有せず読み・書きだけを宣言する。
    #[test]
    fn legacy_attach_declares_reads_and_writes_without_owning() {
        let dir = scratch("legacy");
        let config = legacy(&dir, "pdu_config.json", "Rig", &[(0, "cmd", 136)], &[(1, "attitude", 72)]);
        let ports = extract(&[PduEntry { path: config, usage: Usage::Attach }]);
        assert!(ports.problems.is_empty(), "{:?}", ports.problems);
        assert!(ports.registrations.is_empty());
        assert_eq!(ports.access.iter().map(|a| (a.channel.channel_id, a.access)).collect::<Vec<_>>(), vec![(0, Access::Read), (1, Access::Write)]);
    }

    /// legacy を登録に使うと、reader も含めて作る（Core と同じ）。
    #[test]
    fn legacy_register_owns_readers_too() {
        let dir = scratch("legacy-register");
        let config = legacy(&dir, "pdu_config.json", "Rig", &[(0, "cmd", 136)], &[(1, "attitude", 72)]);
        let ports = extract(&[PduEntry { path: config, usage: Usage::Register }]);
        assert_eq!(ports.registrations.len(), 2);
    }

    /// service 設定は pdu_config_path をたどらず、記録だけする。RPC 定義があればカバレッジを下げる。
    #[test]
    fn service_is_recorded_without_following_and_lowers_coverage_for_rpc() {
        let dir = scratch("service");
        legacy(&dir, "pdu_config.json", "Rig", &[(0, "cmd", 136)], &[]);
        let empty = write(&dir, "service.json", json!({ "pduMetaDataSize": 24, "pdu_config_path": "service.json", "services": [] }));
        let ports = extract(&[PduEntry { path: empty, usage: Usage::Attach }]);
        assert!(ports.problems.is_empty(), "{:?}", ports.problems);
        assert!(ports.access.is_empty(), "自分自身を指していても、たどらない");
        assert_eq!(ports.files.len(), 1);
        assert_eq!(ports.coverage, Coverage::Complete);

        let rpc = write(&dir, "service-rpc.json", json!({ "pdu_config_path": "pdu_config.json", "services": [{ "name": "Service/Add" }] }));
        assert_eq!(extract(&[PduEntry { path: rpc, usage: Usage::Attach }]).coverage, Coverage::Partial);
    }

    /// paths があれば、ほかのキー（services など）があっても compact として読む。
    #[test]
    fn paths_decides_compact_even_with_service_keys() {
        let dir = scratch("compact-services");
        compact_owner(&dir, "Rig", &[(0, "cmd", 136)]);
        let custom = write(&dir, "custom.json", json!({ "paths": [{ "id": "default", "path": "Rig-pdutypes.json" }], "robots": [{ "name": "Rig", "pdutypes_id": "default" }], "services": [] }));
        assert_eq!(extract(&[PduEntry { path: custom, usage: Usage::Register }]).registrations.len(), 1);
    }

    /// 使われない paths[] のファイルも読み、壊れていれば問題にする。id の重複も問題にする。
    #[test]
    fn compact_checks_every_listed_types_file_and_duplicate_ids() {
        let dir = scratch("compact-unused");
        compact_owner(&dir, "Rig", &[(0, "cmd", 136)]);
        std::fs::write(dir.join("broken-pdutypes.json"), b"[ not json").unwrap();
        let unused = write(&dir, "unused.json", json!({ "paths": [{ "id": "default", "path": "Rig-pdutypes.json" }, { "id": "spare", "path": "broken-pdutypes.json" }], "robots": [{ "name": "Rig", "pdutypes_id": "default" }] }));
        assert_eq!(extract(&[PduEntry { path: unused, usage: Usage::Register }]).problems.len(), 1);
        let duplicate = write(&dir, "duplicate.json", json!({ "paths": [{ "id": "default", "path": "Rig-pdutypes.json" }, { "id": "default", "path": "Rig-pdutypes.json" }], "robots": [{ "name": "Rig", "pdutypes_id": "default" }] }));
        assert_eq!(extract(&[PduEntry { path: duplicate, usage: Usage::Register }]).problems.len(), 1);
    }

    /// readers・writers が配列でなければ、空として通さず問題にする。
    #[test]
    fn non_array_legacy_lists_are_problems() {
        let dir = scratch("legacy-list");
        for list in ["shm_pdu_readers", "shm_pdu_writers", "rpc_pdu_readers", "rpc_pdu_writers"] {
            let config = write(&dir, "pdu_config.json", json!({ "robots": [{ "name": "Rig", list: "invalid" }] }));
            assert_eq!(extract(&[PduEntry { path: config, usage: Usage::Attach }]).problems.len(), 1, "{list}");
        }
    }

    /// BOM 付きは、Python が読む Attach では問題、Core が読む Register では通す。
    #[test]
    fn bom_is_a_problem_only_for_python_consumers() {
        let dir = scratch("bom");
        let config = legacy(&dir, "pdu_config.json", "Rig", &[(0, "cmd", 136)], &[]);
        let mut bytes = b"\xef\xbb\xbf".to_vec();
        bytes.extend(std::fs::read(&config).unwrap());
        std::fs::write(&config, bytes).unwrap();
        assert_eq!(extract(&[PduEntry { path: config.clone(), usage: Usage::Attach }]).problems.len(), 1);
        let registered = extract(&[PduEntry { path: config, usage: Usage::Register }]);
        assert!(registered.problems.is_empty() && registered.registrations.len() == 1);
    }

    /// ファイルがない、壊れている、形式が違うときは問題として残し、黙って空にしない。
    #[test]
    fn missing_or_malformed_files_are_problems() {
        let dir = scratch("broken");
        std::fs::write(dir.join("broken.json"), b"{ not json").unwrap();
        let other = write(&dir, "other.json", json!({ "max_angle_rad": 0.35 }));
        let dangling = write(&dir, "custom.json", json!({ "paths": [{ "id": "default", "path": "missing-pdutypes.json" }], "robots": [{ "name": "Rig", "pdutypes_id": "default" }] }));
        for path in [dir.join("absent.json"), dir.join("broken.json"), other, dangling] {
            let ports = extract(&[PduEntry { path: path.clone(), usage: Usage::Register }]);
            assert_eq!(ports.problems.len(), 1, "{}: {:?}", path.display(), ports.problems);
        }
    }

    /// 設定を書き換えると、記録するハッシュが変わる。
    #[test]
    fn digest_changes_with_the_file_content() {
        let dir = scratch("digest");
        let config = legacy(&dir, "pdu_config.json", "Rig", &[(0, "cmd", 136)], &[]);
        let before = extract(&[PduEntry { path: config.clone(), usage: Usage::Attach }]).files;
        legacy(&dir, "pdu_config.json", "Rig", &[(0, "cmd", 144)], &[]);
        let after = extract(&[PduEntry { path: config, usage: Usage::Attach }]).files;
        assert_eq!(before.len(), 1);
        assert_ne!(before[0].sha256, after[0].sha256);
    }
}
