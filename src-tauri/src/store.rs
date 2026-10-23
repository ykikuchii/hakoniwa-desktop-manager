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
    /// v1 の移行が途中で止まった。`app.json` を書くと次の起動で移行をやり直さなくなる
    /// ので、この間は書かない。
    pub migration_pending: bool,
    /// この起動で移行した v1 のファイル（[`file_identity`]）と、そこから作ったプロジェクト。
    /// 移行で消費したファイルを `--import` で指定されたときに、取り込み直さずにそのプロジェクトを選ぶ。
    pub migrated_now: Option<(PathBuf, String)>,
}

/// ライブラリへの書き込みの下書き。プロジェクトと合わせて検証してから
/// [`Store::commit`] でまとめて反映する。検証に落ちた変更が保存に残らないようにするため。
pub struct LibraryDraft {
    pub library: Library,
    writes: Vec<LibraryAsset>,
    /// 内容を変えたため新しい id に置き換えたアセット。反映後にどこからも参照されて
    /// いなければ片付ける。
    superseded: Vec<String>,
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
        let mut store = Self { root: root.to_path_buf(), library: Library::new(), projects: BTreeMap::new(), app: AppConfig::new(), warnings: Vec::new(), migration_pending: false, migrated_now: None };
        for directory in [store.assets_dir(), store.projects_dir()] {
            if let Err(error) = fs::create_dir_all(&directory) {
                store.warnings.push(format!("{} を作れません: {error}", directory.display()));
            }
        }
        let app_path = store.app_path();
        let v1_path = root.join(V1_WORKSPACE_FILE);
        if !app_path.exists() && v1_path.is_file() {
            store.migration_pending = !store.migrate_v1(&v1_path);
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
            // 移行が途中なら書かない。次の起動で移行をやり直すとき、余分な空のプロジェクトが残るため。
            if !self.migration_pending {
                if let Err(error) = self.save_project(&project) {
                    self.warnings.push(error);
                }
            }
            self.projects.insert(project.id.clone(), project);
        }
        let active_is_valid = self.app.active_project_id.as_ref().is_some_and(|id| self.projects.contains_key(id));
        if !active_is_valid {
            let mut candidates: Vec<&Project> = self.projects.values().collect();
            candidates.sort_by(|a, b| b.last_opened_at.cmp(&a.last_opened_at).then_with(|| a.name.cmp(&b.name)));
            self.app.active_project_id = candidates.first().map(|project| project.id.clone());
            if !self.migration_pending {
                if let Err(error) = self.save_app() {
                    self.warnings.push(error);
                }
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
    ///
    /// 移行が終わったか、移行するものがなかった（読めない v1 を隔離した）ときに true。
    /// 途中で止まったときは false で、呼び出し側は `app.json` を書かない。
    fn migrate_v1(&mut self, v1_path: &Path) -> bool {
        let bytes = match fs::read(v1_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.warnings.push(format!("{} を読めません（次の起動で移行をやり直します）: {error}", v1_path.display()));
                return false;
            }
        };
        let workspace = match serde_json::from_slice::<Workspace>(&bytes).map_err(|error| error.to_string()).and_then(|workspace| workspace.validate().map(|()| workspace)) {
            Ok(workspace) => workspace,
            Err(reason) => {
                self.quarantine(v1_path, &format!("v1 のワークスペースとして読めないため移行しませんでした: {reason}"));
                return true;
            }
        };
        let (assets, mut project) = split_v1_workspace(&workspace);
        // 移行する構成は、これまでそのまま起動していたものなので、今の内容で承認しておく。
        let library: Library = assets.iter().map(|asset| (asset.id.clone(), asset.clone())).collect();
        if let Err(error) = crate::check::accept_all(&mut project, &library) {
            self.warnings.push(format!("移行した構成を承認できませんでした（接続チェックで承認してください）: {error}"));
        }
        let result = (|| {
            for asset in &assets {
                self.save_asset(asset)?;
            }
            self.save_project(&project)?;
            self.backup_v1(v1_path)?;
            let app = AppConfig { schema_version: STORE_SCHEMA_VERSION, active_project_id: Some(project.id.clone()), migrated_from_v1: Some(MigrationRecord { source_sha256: hex::encode(Sha256::digest(&bytes)), at: Utc::now() }) };
            self.write_app(&app)?;
            self.app = app;
            Ok::<(), String>(())
        })();
        match result {
            Ok(()) => {
                self.warnings.push(format!("v1 のワークスペース「{}」を、プロジェクトとライブラリに移行しました。", workspace.name));
                self.migrated_now = file_identity(v1_path).map(|identity| (identity, project.id.clone()));
                true
            }
            Err(error) => {
                self.warnings.push(format!("v1 の移行が途中で止まりました。アプリを再起動すると移行をやり直します。それまで設定の保存の一部はできません: {error}"));
                false
            }
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
        // 置き換えるときは、同じメンバーが今使っているアセットを引き継ぐ。内容が同じなら
        // 書かないので、取り込み直すたびにライブラリが増えることはない。
        let previous: BTreeMap<String, String> = self.projects.get(&project.id).map(|old| old.members.iter().map(|member| (member.id.clone(), member.asset_id.clone())).collect()).unwrap_or_default();
        let mut draft = self.draft();
        for (mut asset, member) in assets.into_iter().zip(project.members.iter_mut()) {
            if let Some(id) = previous.get(&member.id) {
                asset.id = id.clone();
            }
            member.asset_id = Self::stage_asset(&mut draft, asset);
        }
        // 取り込みは利用者が構成を明示的に入れる操作なので、今の内容で承認する。
        crate::check::accept_all(&mut project, &draft.library)?;
        self.commit(draft, project.clone())?;
        if consume {
            self.backup_v1(path)?;
        }
        Ok(ImportOutcome { project_id: project.id, replaced })
    }

    /// プロジェクトのメンバーを、今の内容で承認して保存する（全員）。
    pub fn accept(&mut self, project_id: &str) -> Result<(), String> {
        let mut project = self.projects.get(project_id).cloned().ok_or_else(|| "そのプロジェクトはありません。".to_owned())?;
        crate::check::accept_all(&mut project, &self.library)?;
        self.commit(self.draft(), project)
    }


    pub fn draft(&self) -> LibraryDraft {
        LibraryDraft { library: self.library.clone(), writes: Vec::new(), superseded: Vec::new() }
    }

    /// アセットを下書きに置き、使う id を返す（コピーオンライト）。
    ///
    /// 同じ id で同じ内容のものがあれば、そのまま使う。内容が違えば、元を書き換えずに
    /// 新しい id で置く。元を参照しているほかのメンバー（同じプロジェクトでも、ほかの
    /// プロジェクトでも）は、元のまま変わらない。
    pub fn stage_asset(draft: &mut LibraryDraft, mut asset: LibraryAsset) -> String {
        if let Some(existing) = draft.library.get(&asset.id) {
            if same_content(existing, &asset) {
                return asset.id;
            }
            draft.superseded.push(asset.id.clone());
            asset.id = Uuid::new_v4().to_string();
        }
        asset.updated_at = Utc::now();
        let id = asset.id.clone();
        draft.library.insert(id.clone(), asset.clone());
        draft.writes.push(asset);
        id
    }

    /// 下書きとプロジェクトを検証してから保存し、成功したらメモリ上の状態も差し替える。
    ///
    /// 書く順は、新しいアセット → プロジェクト。変わったアセットは新しい id なので、
    /// プロジェクトを保存する前に落ちても、既存の参照先は書き換わらない（残るのは
    /// どこからも参照されないアセットだけ）。最後に、置き換えられて参照されなくなった
    /// アセットを片付ける。
    pub fn commit(&mut self, draft: LibraryDraft, project: Project) -> Result<(), String> {
        for asset in &draft.writes {
            asset.validate()?;
        }
        project.validate(&draft.library)?;
        let mut written = Vec::new();
        let saved = draft.writes.iter().try_for_each(|asset| self.save_asset(asset).map(|()| written.push(asset.clone()))).and_then(|()| self.save_project(&project));
        if let Err(error) = saved {
            // 書けたアセットはディスクに残り、次の起動で読み込まれる。今の一覧にも出しておく
            // （どこからも参照されていないアセットとして見え、消せる）。
            for asset in written {
                self.library.insert(asset.id.clone(), asset);
            }
            return Err(error);
        }
        self.library = draft.library;
        self.projects.insert(project.id.clone(), project);
        self.remove_unreferenced(&draft.superseded);
        Ok(())
    }

    /// プロジェクトを変えずに、ライブラリへの書き込みだけを反映する。
    pub fn commit_library(&mut self, draft: LibraryDraft) -> Result<(), String> {
        for asset in &draft.writes {
            asset.validate()?;
        }
        for asset in &draft.writes {
            self.save_asset(asset)?;
        }
        self.library = draft.library;
        self.remove_unreferenced(&draft.superseded);
        Ok(())
    }

    /// どのプロジェクトも使っていないアセットを消す。使われていれば、使っているプロジェクトを挙げて拒否する。
    pub fn remove_asset(&mut self, id: &str) -> Result<(), String> {
        let users: Vec<&str> = self.projects.values().filter(|project| project.members.iter().any(|member| member.asset_id == id)).map(|project| project.name.as_str()).collect();
        if !users.is_empty() {
            return Err(format!("このアセットは、プロジェクト {} が使っているので消せません。", users.join("、")));
        }
        if !self.library.contains_key(id) {
            return Err("そのアセットはライブラリにありません。".to_owned());
        }
        self.ensure_writable()?;
        let path = self.assets_dir().join(format!("{}.json", file_stem(id)?));
        fs::remove_file(&path).map_err(|error| format!("{} を消せません: {error}", path.display()))?;
        self.library.remove(id);
        Ok(())
    }

    /// 置き換えられたアセットのうち、どこからも参照されていないものを片付ける。
    /// ファイルを消せなかったものは一覧に残し、警告に出す（消したことにすると、次の起動で
    /// 読み込まれて一覧と食い違う）。
    fn remove_unreferenced(&mut self, superseded: &[String]) {
        for id in superseded {
            let referenced = self.projects.values().any(|candidate| candidate.members.iter().any(|member| &member.asset_id == id));
            if referenced {
                continue;
            }
            let removed = file_stem(id).and_then(|stem| {
                let path = self.assets_dir().join(format!("{stem}.json"));
                match fs::remove_file(&path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(format!("使われなくなったアセット {} を消せませんでした（ライブラリから消してください）: {error}", path.display())),
                }
            });
            match removed {
                Ok(()) => {
                    self.library.remove(id);
                }
                Err(warning) => self.warnings.push(warning),
            }
        }
    }

    pub fn save_asset(&self, asset: &LibraryAsset) -> Result<(), String> {
        self.ensure_writable()?;
        write_json(&self.assets_dir().join(format!("{}.json", file_stem(&asset.id)?)), asset)
    }

    pub fn save_project(&self, project: &Project) -> Result<(), String> {
        self.ensure_writable()?;
        write_json(&self.projects_dir().join(format!("{}.json", file_stem(&project.id)?)), project)
    }

    pub fn save_app(&self) -> Result<(), String> {
        self.write_app(&self.app)
    }

    /// `app` を `app.json` に書く。メモリ上の値は変えない。呼び出し側は、成功してから差し替える。
    pub fn write_app(&self, app: &AppConfig) -> Result<(), String> {
        self.ensure_writable()?;
        write_json(&self.app_path(), app)
    }

    /// 移行が途中なら、何も書かせない。次の起動の移行のやり直しは v1 からプロジェクトを
    /// 作り直すので、その間に保存した編集は上書きされて消えるため。
    fn ensure_writable(&self) -> Result<(), String> {
        if self.migration_pending {
            return Err("v1 の移行が途中のため、変更を保存できません。アプリを再起動して移行をやり直してください。".to_owned());
        }
        Ok(())
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

/// ファイルの同一性を比べるためのパス。親ディレクトリを実体に解決し（相対パスは作業
/// ディレクトリ基準）、Windows では大文字小文字を揃える。ファイル自体が消えていても
/// （移行で退避した後でも）求められるよう、ファイル名は解決しない。
pub fn file_identity(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let resolved = fs::canonicalize(parent).ok()?.join(name);
    Some(if cfg!(windows) { PathBuf::from(resolved.to_string_lossy().to_lowercase()) } else { resolved })
}

/// 更新時刻を除いて、同じ内容か。
fn same_content(a: &LibraryAsset, b: &LibraryAsset) -> bool {
    let strip = |asset: &LibraryAsset| {
        let mut value = serde_json::to_value(asset).unwrap_or_default();
        if let Some(object) = value.as_object_mut() {
            object.remove("updated_at");
        }
        value
    };
    strip(a) == strip(b)
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
        assert!(store.projects[store.active_project_id()].members.iter().all(|member| member.accepted.is_some()), "移行した構成は承認済み");

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
        assert!(store.projects[&outcome.project_id].members.iter().all(|member| member.accepted.is_some()), "取り込んだ構成は承認済み");
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

        // 取り込み直しても、同じ内容ならアセットは増えない。
        let assets = store.library.len();
        store.import_v1(&source, true, false).unwrap();
        assert_eq!(store.library.len(), assets);

        // 置き換えでなければ、複製として足す。
        let copy = store.import_v1(&source, false, false).unwrap();
        assert_ne!(copy.project_id, outcome.project_id);
        assert!(store.projects[&copy.project_id].origin.is_none());
    }

    /// 内容を変えたアセットは新しい id に置き、元を参照するほかのメンバーは変えない。
    /// 検証に落ちれば何も保存しない。参照されなくなった元は片付ける。
    #[test]
    fn staged_changes_copy_and_commit_only_after_validation() {
        let dir = scratch("cow");
        write_v1(&dir, &v1_demo());
        let mut store = Store::open(&dir);
        let id = store.active_project_id().to_owned();
        let original = store.projects[&id].members[0].asset_id.clone();

        // 同じプロジェクトの 2 つ目のメンバーが同じアセットを使う。
        let mut project = store.projects[&id].clone();
        let mut twin = project.members[0].clone();
        twin.id = "twin".to_owned();
        project.members.push(twin);
        store.commit(store.draft(), project.clone()).unwrap();

        // 1 つ目だけを変える。
        let mut draft = store.draft();
        let mut edited = store.library[&original].clone();
        edited.name = "edited".to_owned();
        let edited_id = Store::stage_asset(&mut draft, edited.clone());
        assert_ne!(edited_id, original);
        project.members[0].asset_id = edited_id.clone();
        store.commit(draft, project.clone()).unwrap();
        assert_eq!(store.library[&original].name, "Gimbal rig (owner)", "twin が参照する元は変えない");
        assert_eq!(store.library[&edited_id].name, "edited");

        // 同じ内容なら id は変わらず、何も書かない。
        let mut draft = store.draft();
        assert_eq!(Store::stage_asset(&mut draft, store.library[&edited_id].clone()), edited_id);
        assert!(draft.writes.is_empty());

        // 検証に落ちる変更は、アセットも含めて何も残さない。
        let files_before = json_files(&dir.join("library").join("assets"));
        let mut draft = store.draft();
        let mut broken = store.library[&edited_id].clone();
        broken.name = "should not persist".to_owned();
        let broken_id = Store::stage_asset(&mut draft, broken);
        let mut invalid = project.clone();
        invalid.members[0].asset_id = broken_id;
        invalid.members[1].id = invalid.members[0].id.clone();
        assert!(store.commit(draft, invalid).is_err());
        assert_eq!(json_files(&dir.join("library").join("assets")), files_before);
        assert!(store.library.values().all(|asset| asset.name != "should not persist"));

        // twin を外してから元を置き換えると、参照されなくなった元は片付く。
        project.members.retain(|member| member.id != "twin");
        store.commit(store.draft(), project.clone()).unwrap();
        let mut draft = store.draft();
        let mut again = store.library[&original].clone();
        again.name = "replaced".to_owned();
        let mut with_original = project.clone();
        with_original.members[0].asset_id = original.clone();
        store.commit(store.draft(), with_original.clone()).unwrap();
        with_original.members[0].asset_id = Store::stage_asset(&mut draft, again);
        store.commit(draft, with_original).unwrap();
        assert!(!store.library.contains_key(&original));
        assert!(!dir.join("library").join("assets").join(format!("{original}.json")).exists());
    }

    /// 移行が途中で止まったら app.json を書かず、次に開くとやり直す。
    #[test]
    fn a_failed_migration_is_retried_on_the_next_open() {
        let dir = scratch("migrate-retry");
        write_v1(&dir, &v1_demo());
        // projects を同名のファイルにして、プロジェクトの保存を失敗させる。
        fs::write(dir.join("projects"), b"not a directory").unwrap();
        let store = Store::open(&dir);
        assert!(store.migration_pending);
        assert!(!dir.join("app.json").exists());
        assert!(dir.join(V1_WORKSPACE_FILE).exists(), "退避前に止まったので v1 は残る");
        assert!(store.write_app(&store.app).is_err());
        assert!(store.save_project(&Project::empty("x".to_owned(), "x".to_owned())).is_err(), "移行が途中の間は編集も保存しない");

        fs::remove_file(dir.join("projects")).unwrap();
        let reopened = Store::open(&dir);
        assert!(!reopened.migration_pending);
        assert!(dir.join("app.json").exists());
        assert_eq!(resolved_active(&reopened), serde_json::to_value(v1_demo()).unwrap());
    }

    /// 同じファイルは、`..` を含む表記でも大文字小文字が違っても同じとみなす。消えた後でも求められる。
    #[test]
    fn file_identity_ignores_spelling() {
        let dir = scratch("identity");
        let file = dir.join("workspace.json");
        fs::write(&file, b"{}").unwrap();
        let absolute = file_identity(&file).unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        assert_eq!(file_identity(&dir.join("sub").join("..").join("workspace.json")).as_ref(), Some(&absolute));
        fs::remove_file(&file).unwrap();
        assert_eq!(file_identity(&file).as_ref(), Some(&absolute), "ファイルが消えた後でも同じ");
        if cfg!(windows) {
            let upper = PathBuf::from(file.to_string_lossy().to_uppercase());
            assert_eq!(file_identity(&upper).as_ref(), Some(&absolute));
        }
        assert_ne!(file_identity(&dir.join("other.json")).as_ref(), Some(&absolute));
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
