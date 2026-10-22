//! ライブラリとプロジェクトの保存。
//!
//! ```text
//! <data>/app.json
//! <data>/library/assets/<asset-id>.json
//! <data>/projects/<project-id>.json
//! <data>/backup/workspace.v1.<時刻>.json     … 移行・取り込みした v1
//! <data>/quarantine/<元の名前>.<時刻>        … 読めなかったファイル
//! ```
//!
//! 1 ファイルずつ一時ファイルに書いてから置き換える。複数のファイルにまたがる更新は
//! ライブラリ → プロジェクト → `app.json` の順に書くので、途中で落ちても残るのは
//! どこからも参照されないアセットだけになる。


use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::model::{split_v1_workspace, v1_origin, AppConfig, Library, LibraryAsset, MigrationRecord, Project, STORE_SCHEMA_VERSION};
use crate::types::Workspace;

/// v1 のワークスペースのファイル名。
pub const V1_WORKSPACE_FILE: &str = "workspace.json";

pub struct Store {
    pub root: PathBuf,
    pub library: Library,
    pub projects: BTreeMap<String, Project>,
    pub app: AppConfig,
    /// 読み込みと移行で起きたこと。画面に出す。
    pub warnings: Vec<String>,
}

/// v1 を取り込んだ結果。
#[derive(Debug, PartialEq, Eq)]
pub struct ImportOutcome {
    pub project_id: String,
    /// 同じ出所のプロジェクトを置き換えたか。
    pub replaced: bool,
}

impl Store {
    /// 保存場所を開く。`app.json` がなく v1 の `workspace.json` があれば、先に移行する。
    pub fn open(root: &Path) -> Self {
        let mut store = Self { root: root.to_path_buf(), library: Library::new(), projects: BTreeMap::new(), app: AppConfig::new(), warnings: Vec::new() };
        for directory in [store.assets_dir(), store.projects_dir()] {
            if let Err(error) = fs::create_dir_all(&directory) {
                store.warnings.push(format!("{} を作れません: {error}", directory.display()));
            }
        }
        let app_path = store.app_path();
        let v1_path = root.join(V1_WORKSPACE_FILE);
        if !app_path.exists() && v1_path.is_file() {
            store.migrate_v1(&v1_path);
        }
        store.load();
        if app_path.exists() && v1_path.is_file() {
            store.warnings.push(format!("移行済みの保存場所に、v1 の {} があります。取り込むには「v1 を取り込む」を使ってください。", v1_path.display()));
        }
        store.ensure_active_project();
        store
    }

    fn assets_dir(&self) -> PathBuf {
        self.root.join("library").join("assets")
    }

    fn projects_dir(&self) -> PathBuf {
        self.root.join("projects")
    }

    fn app_path(&self) -> PathBuf {
        self.root.join("app.json")
    }

    fn load(&mut self) {
        if let Some(app) = self.read_json::<AppConfig>(&self.app_path()) {
            if app.schema_version == STORE_SCHEMA_VERSION {
                self.app = app;
            } else {
                self.quarantine(&self.app_path(), &format!("形式 v{} は未対応です", app.schema_version));
            }
        }
        for path in json_files(&self.assets_dir()) {
            let Some(asset) = self.read_json::<LibraryAsset>(&path) else { continue };
            match asset.validate().and_then(|()| check_file_name(&path, &asset.id)) {
                Ok(()) => {
                    self.library.insert(asset.id.clone(), asset);
                }
                Err(reason) => self.quarantine(&path, &reason),
            }
        }
        for path in json_files(&self.projects_dir()) {
            let Some(project) = self.read_json::<Project>(&path) else { continue };
            if let Err(reason) = check_file_name(&path, &project.id) {
                self.quarantine(&path, &reason);
                continue;
            }
            // 参照先のアセットが隔離されたなどで検証に通らなくても、プロジェクト自体は残す。
            // 隔離すると利用者の組み合わせが丸ごと見えなくなる。開こうとしたときに理由を出す。
            if let Err(reason) = project.validate(&self.library) {
                self.warnings.push(format!("プロジェクト「{}」は、このままでは開けません: {reason}", project.name));
            }
            self.projects.insert(project.id.clone(), project);
        }
    }

    /// アクティブなプロジェクトを決める。1 つもなければ空のプロジェクトを作る。
    fn ensure_active_project(&mut self) {
        if self.projects.is_empty() {
            let project = Project::empty(Uuid::new_v4().to_string(), "新しいプロジェクト".to_owned());
            if let Err(error) = self.save_project(&project) {
                self.warnings.push(error);
            }
            self.projects.insert(project.id.clone(), project);
        }
        let active_is_valid = self.app.active_project_id.as_ref().is_some_and(|id| self.projects.contains_key(id));
        if !active_is_valid {
            let mut candidates: Vec<&Project> = self.projects.values().collect();
            candidates.sort_by(|a, b| b.last_opened_at.cmp(&a.last_opened_at).then_with(|| a.name.cmp(&b.name)));
            self.app.active_project_id = candidates.first().map(|project| project.id.clone());
            if let Err(error) = self.save_app() {
                self.warnings.push(error);
            }
        }
    }

    pub fn active_project_id(&self) -> &str {
        self.app.active_project_id.as_deref().unwrap_or_default()
    }

    /// v1 の `workspace.json` を移行する。
    ///
    /// ライブラリ → プロジェクト → v1 の退避 → `app.json` の順に行う。id は v1 の内容から
    /// 決まるので、途中で落ちて次に起動したときにやり直しても同じファイルを書くだけになる。
    /// `app.json` は最後に書くので、それがある時点で移行は終わっている。
    fn migrate_v1(&mut self, v1_path: &Path) {
        let bytes = match fs::read(v1_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.warnings.push(format!("{} を読めません: {error}", v1_path.display()));
                return;
            }
        };
        let workspace = match serde_json::from_slice::<Workspace>(&bytes).map_err(|error| error.to_string()).and_then(|workspace| workspace.validate().map(|()| workspace)) {
            Ok(workspace) => workspace,
            Err(reason) => {
                self.quarantine(v1_path, &format!("v1 のワークスペースとして読めないため移行しませんでした: {reason}"));
                return;
            }
        };
        let (assets, project) = split_v1_workspace(&workspace);
        let result = (|| {
            for asset in &assets {
                self.save_asset(asset)?;
            }
            self.save_project(&project)?;
            self.backup_v1(v1_path)?;
            self.app = AppConfig { schema_version: STORE_SCHEMA_VERSION, active_project_id: Some(project.id.clone()), migrated_from_v1: Some(MigrationRecord { source_sha256: hex::encode(Sha256::digest(&bytes)), at: Utc::now() }) };
            self.save_app()
        })();
        match result {
            Ok(()) => self.warnings.push(format!("v1 のワークスペース「{}」を、プロジェクトとライブラリに移行しました。", workspace.name)),
            Err(error) => self.warnings.push(format!("v1 の移行が途中で止まりました（次の起動でやり直します）: {error}")),
        }
    }

    /// v1 のワークスペースを、明示的に取り込む。
    ///
    /// `replace` のとき、同じ出所のプロジェクトがあれば、その id のまま中身を置き換える。
    /// なければ、または `replace` でなければ、新しいプロジェクトとして足す。ライブラリへの
    /// 書き込みはコピーオンライトで、ほかのプロジェクトが使っているアセットは複製する。
    /// `consume` のときは、取り込んだファイルを `backup/` に移す。
    pub fn import_v1(&mut self, path: &Path, replace: bool, consume: bool) -> Result<ImportOutcome, String> {
        let bytes = fs::read(path).map_err(|error| format!("{} を読めません: {error}", path.display()))?;
        let workspace: Workspace = serde_json::from_slice(&bytes).map_err(|error| format!("{} は v1 のワークスペースとして読めません: {error}", path.display()))?;
        workspace.validate()?;
        let origin = v1_origin(&workspace);
        let (assets, mut project) = split_v1_workspace(&workspace);
        let existing = replace.then(|| self.projects.values().find(|candidate| candidate.origin.as_deref() == Some(origin.as_str())).map(|candidate| candidate.id.clone())).flatten();
        let replaced = existing.is_some();
        project.id = match existing {
            Some(id) => id,
            None if self.projects.contains_key(&project.id) => Uuid::new_v4().to_string(),
            None => project.id,
        };
        if !replaced {
            // 複製として足すときは、出所を引き継がない。次の置き換えの相手が曖昧になる。
            if self.projects.values().any(|candidate| candidate.origin.as_deref() == Some(origin.as_str())) {
                project.origin = None;
            }
        }
        for (asset, member) in assets.into_iter().zip(project.members.iter_mut()) {
            member.asset_id = self.write_asset_for(asset, &project.id)?;
        }
        project.validate(&self.library)?;
        self.save_project(&project)?;
        self.projects.insert(project.id.clone(), project.clone());
        if consume {
            self.backup_v1(path)?;
        }
        Ok(ImportOutcome { project_id: project.id, replaced })
    }

    /// ライブラリにアセットを書く（コピーオンライト）。
    ///
    /// 同じ id のアセットを `project_id` 以外のプロジェクトも使っていれば、新しい id で
    /// 複製して書く。ほかのプロジェクトが黙って変わらないようにするため。返り値は、
    /// 書いたアセットの id。
    pub fn write_asset_for(&mut self, mut asset: LibraryAsset, project_id: &str) -> Result<String, String> {
        let shared = self.projects.values().any(|project| project.id != project_id && project.members.iter().any(|member| member.asset_id == asset.id));
        if shared {
            asset.id = Uuid::new_v4().to_string();
        }
        asset.updated_at = Utc::now();
        asset.validate()?;
        self.save_asset(&asset)?;
        let id = asset.id.clone();
        self.library.insert(id.clone(), asset);
        Ok(id)
    }

    pub fn save_asset(&self, asset: &LibraryAsset) -> Result<(), String> {
        write_json(&self.assets_dir().join(format!("{}.json", file_stem(&asset.id)?)), asset)
    }

    pub fn save_project(&self, project: &Project) -> Result<(), String> {
        write_json(&self.projects_dir().join(format!("{}.json", file_stem(&project.id)?)), project)
    }

    pub fn save_app(&self) -> Result<(), String> {
        write_json(&self.app_path(), &self.app)
    }

    fn backup_v1(&self, path: &Path) -> Result<(), String> {
        let directory = self.root.join("backup");
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let target = directory.join(format!("workspace.v1.{}.json", timestamp()));
        fs::rename(path, &target).or_else(|_| fs::copy(path, &target).and_then(|_| fs::remove_file(path))).map_err(|error| format!("{} を退避できません: {error}", path.display()))
    }

    fn read_json<T: serde::de::DeserializeOwned>(&mut self, path: &Path) -> Option<T> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                self.warnings.push(format!("{} を読めません: {error}", path.display()));
                return None;
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(value) => Some(value),
            Err(error) => {
                self.quarantine(path, &format!("JSON として読めません: {error}"));
                None
            }
        }
    }

    /// 読めないファイルを `quarantine/` に移し、理由を警告に残す。消さないので、利用者が
    /// 中身を確かめて戻せる。
    fn quarantine(&mut self, path: &Path, reason: &str) {
        let directory = self.root.join("quarantine");
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let target = directory.join(format!("{name}.{}", timestamp()));
        let moved = fs::create_dir_all(&directory).and_then(|()| fs::rename(path, &target));
        match moved {
            Ok(()) => self.warnings.push(format!("{} を {} に隔離しました: {reason}", path.display(), target.display())),
            Err(error) => self.warnings.push(format!("{} は使えません（隔離にも失敗: {error}）: {reason}", path.display())),
        }
    }
}

fn json_files(directory: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|extension| extension == "json"))
        .collect();
    files.sort();
    files
}

/// ファイル名とその中の id が揃っているか。揃っていないと、保存で別のファイルに書いてしまう。
fn check_file_name(path: &Path, id: &str) -> Result<(), String> {
    let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    if stem == file_stem(id)? {
        Ok(())
    } else {
        Err(format!("ファイル名（{stem}）と中の ID（{id}）が一致しません"))
    }
}

/// id をファイル名に使えるか確かめる。区切り文字などを含む id は、別の場所への書き込みに
/// なりうるので拒否する。
fn file_stem(id: &str) -> Result<&str, String> {
    let usable = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) && !id.starts_with('.');
    if usable {
        Ok(id)
    } else {
        Err(format!("ID「{id}」はファイル名に使えない文字を含みます"))
    }
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let content = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, content).map_err(|error| format!("{} に書けません: {error}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|error| format!("{} を置き換えられません: {error}", path.display()))
}

fn timestamp() -> String {
    Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::resolve;
    use crate::resolve::tests::v1_demo;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hdm-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_v1(dir: &Path, workspace: &Workspace) -> PathBuf {
        let path = dir.join(V1_WORKSPACE_FILE);
        fs::write(&path, serde_json::to_vec_pretty(workspace).unwrap()).unwrap();
        path
    }

    fn resolved_active(store: &Store) -> serde_json::Value {
        serde_json::to_value(resolve(&store.projects[store.active_project_id()], &store.library).unwrap()).unwrap()
    }

    /// v1 を移行すると、v1 は退避され、アクティブなプロジェクトは元と同じワークスペースに解決する。
    #[test]
    fn migrates_v1_into_an_equivalent_project() {
        let dir = scratch("migrate");
        let original = v1_demo();
        write_v1(&dir, &original);
        let store = Store::open(&dir);
        assert_eq!(resolved_active(&store), serde_json::to_value(&original).unwrap());
        assert!(!dir.join(V1_WORKSPACE_FILE).exists());
        assert_eq!(json_files(&dir.join("backup")).len(), 1);
        assert!(store.app.migrated_from_v1.is_some());
        assert_eq!(store.library.len(), 6);

        // 開き直しても同じ。移行はやり直さない。
        let reopened = Store::open(&dir);
        assert_eq!(resolved_active(&reopened), serde_json::to_value(&original).unwrap());
        assert_eq!(reopened.library.len(), 6);
    }

    /// 移行が途中で止まっても（app.json を書く前）、次に開くとやり直して同じ結果になる。
    #[test]
    fn interrupted_migration_is_redone_to_the_same_result() {
        let dir = scratch("migrate-interrupted");
        let original = v1_demo();
        let v1 = write_v1(&dir, &original);
        let finished = Store::open(&dir);
        let expected = resolved_active(&finished);
        let asset_files = json_files(&dir.join("library").join("assets"));

        // app.json と退避を取り消し、ライブラリとプロジェクトだけ書かれた状態に戻す。
        fs::remove_file(dir.join("app.json")).unwrap();
        let backup = json_files(&dir.join("backup")).remove(0);
        fs::rename(&backup, &v1).unwrap();
        let redone = Store::open(&dir);
        assert_eq!(resolved_active(&redone), expected);
        assert_eq!(json_files(&dir.join("library").join("assets")), asset_files);
        assert_eq!(redone.projects.len(), 1);
    }

    /// 移行後に現れた v1 は自動では取り込まず、知らせるだけ。
    #[test]
    fn a_v1_file_after_migration_is_reported_not_imported() {
        let dir = scratch("v1-after");
        write_v1(&dir, &v1_demo());
        Store::open(&dir);
        let mut again = v1_demo();
        again.id = "another-run".to_owned();
        write_v1(&dir, &again);
        let store = Store::open(&dir);
        assert_eq!(store.projects.len(), 1);
        assert!(dir.join(V1_WORKSPACE_FILE).exists());
        assert!(store.warnings.iter().any(|warning| warning.contains("v1 を取り込む")), "{:?}", store.warnings);
    }

    /// 壊れたファイルは隔離して警告し、空の状態に黙って戻らない。
    #[test]
    fn broken_files_are_quarantined_with_warnings() {
        let dir = scratch("broken");
        write_v1(&dir, &v1_demo());
        let store = Store::open(&dir);
        let project_id = store.active_project_id().to_owned();
        let asset_id = store.projects[&project_id].members[0].asset_id.clone();
        fs::write(dir.join("library").join("assets").join(format!("{asset_id}.json")), b"{ broken").unwrap();
        fs::write(dir.join("projects").join("stray.json"), serde_json::to_vec(&store.projects[&project_id]).unwrap()).unwrap();

        let reopened = Store::open(&dir);
        assert_eq!(json_files(&dir.join("quarantine")).len(), 0, "隔離先は拡張子を変えるので json_files には出ない");
        assert_eq!(fs::read_dir(dir.join("quarantine")).unwrap().count(), 2);
        assert!(reopened.warnings.iter().filter(|w| w.contains("隔離しました")).count() == 2, "{:?}", reopened.warnings);
        // アセットを失ったプロジェクトは残し、開けない理由を出す。
        assert!(reopened.projects.contains_key(&project_id));
        assert!(reopened.warnings.iter().any(|w| w.contains("このままでは開けません")), "{:?}", reopened.warnings);
        assert_eq!(reopened.active_project_id(), project_id);
    }

    /// 何もない場所を開くと、空のプロジェクトを 1 つ作ってアクティブにする。
    #[test]
    fn an_empty_store_gets_one_empty_project() {
        let dir = scratch("empty");
        let store = Store::open(&dir);
        assert_eq!(store.projects.len(), 1);
        assert!(store.projects.contains_key(store.active_project_id()));
        assert!(dir.join("app.json").exists());
        assert_eq!(Store::open(&dir).projects.len(), 1);
    }

    /// 同じ出所の v1 を取り込み直すと、プロジェクトもライブラリも増えずに置き換わる。
    #[test]
    fn reimporting_the_same_origin_replaces_instead_of_growing() {
        let dir = scratch("reimport");
        let source = scratch("reimport-src").join("workspace.v1.json");
        let mut first = v1_demo();
        fs::write(&source, serde_json::to_vec(&first).unwrap()).unwrap();
        let mut store = Store::open(&dir);
        let outcome = store.import_v1(&source, true, false).unwrap();
        assert!(!outcome.replaced);
        assert!(source.exists(), "consume しなければ元のファイルは残す");

        first.id = "regenerated-uuid".to_owned();
        first.assets[0].name = "Gimbal rig (owner, v2)".to_owned();
        fs::write(&source, serde_json::to_vec(&first).unwrap()).unwrap();
        let again = store.import_v1(&source, true, false).unwrap();
        assert_eq!(again, ImportOutcome { project_id: outcome.project_id.clone(), replaced: true });
        let imported: Vec<_> = store.projects.values().filter(|p| p.origin.is_some()).collect();
        assert_eq!(imported.len(), 1);
        assert_eq!(store.library.len(), 6);
        assert_eq!(store.library[&store.projects[&outcome.project_id].members[0].asset_id].name, "Gimbal rig (owner, v2)");

        // 置き換えでなければ、複製として足す。
        let copy = store.import_v1(&source, false, false).unwrap();
        assert_ne!(copy.project_id, outcome.project_id);
        assert!(store.projects[&copy.project_id].origin.is_none());
    }

    /// ほかのプロジェクトも使っているアセットを書き換えると、複製して書き、ほかは変えない。
    #[test]
    fn writing_a_shared_asset_copies_it() {
        let dir = scratch("cow");
        let source = scratch("cow-src").join("workspace.v1.json");
        fs::write(&source, serde_json::to_vec(&v1_demo()).unwrap()).unwrap();
        let mut store = Store::open(&dir);
        let a = store.import_v1(&source, true, false).unwrap().project_id;
        // 別のプロジェクトに同じアセットを参照させる。
        let mut b = store.projects[&a].clone();
        b.id = "project-b".to_owned();
        b.origin = None;
        store.save_project(&b).unwrap();
        store.projects.insert(b.id.clone(), b);

        let shared_id = store.projects[&a].members[0].asset_id.clone();
        let mut edited = store.library[&shared_id].clone();
        edited.name = "edited in A".to_owned();
        let written = store.write_asset_for(edited.clone(), &a).unwrap();
        assert_ne!(written, shared_id);
        assert_eq!(store.library[&shared_id].name, "Gimbal rig (owner)");
        assert_eq!(store.library[&written].name, "edited in A");

        // 使っているのが自分だけなら、そのまま書き換える。
        let only_b = store.write_asset_for(store.library[&written].clone(), "project-b").unwrap();
        assert_eq!(only_b, written);
    }

    /// 区切り文字を含む id は、別の場所に書かないよう拒否する。
    #[test]
    fn ids_with_path_separators_are_rejected() {
        assert!(file_stem("../evil").is_err());
        assert!(file_stem("a/b").is_err());
        assert!(file_stem("a\\b").is_err());
        assert!(file_stem("").is_err());
        assert!(file_stem("3f1c1f9e-0000-4000-8000-000000000001").is_ok());
    }
}
