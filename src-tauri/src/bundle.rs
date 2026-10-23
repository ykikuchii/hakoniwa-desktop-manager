//! プロジェクトの書き出しと読み込み（`.hakoproj.json`）。
//!
//! 1 つのファイルに、プロジェクトと、そのメンバーが参照するライブラリのアセットを入れる。
//! パスは **基準ルート** の下にあるものだけを `${PROJECT_ROOT}/...` の相対表記にする。
//! ファイルには、ファイルのあるディレクトリから見た基準ルートの位置（`root`）を記録し、
//! 読み込むときはそこから基準ルートを決め直す（利用者が別の場所を選んでもよい）。
//! 基準ルートの外の絶対パス（システムの Python など）は書き換えず、警告に出す。
//!
//! 承認は書き出さない。承認は、その PC のファイルの内容と結び付いているため。

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::model::{Library, LibraryAsset, Project, STORE_SCHEMA_VERSION};
use crate::types::{ExecutionTarget, ProgramSpec};

pub const FORMAT: &str = "hakoniwa-desktop-manager/project";
pub const FORMAT_VERSION: u32 = 1;
pub const ROOT_TOKEN: &str = "${PROJECT_ROOT}";
pub const EXTENSION: &str = ".hakoproj.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub format: String,
    pub format_version: u32,
    pub exported_at: DateTime<Utc>,
    /// ファイルのあるディレクトリから見た基準ルート（`/` 区切りの相対パス。同じなら `.`）。
    pub root: String,
    pub project: Project,
    pub assets: Vec<LibraryAsset>,
}

/// 読み込んで、パスをこの PC の基準ルートに戻したもの。
#[derive(Debug, Clone)]
pub struct Loaded {
    pub bundle: Bundle,
    /// 読んだファイルの内容の sha256。プレビューと取り込みの間に変わっていないかを確かめる。
    pub sha256: String,
    pub root: String,
    pub warnings: Vec<String>,
}

/// ファイルが `.hakoproj.json` 形式か。中身の `format` で判断する。
pub fn is_bundle(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(strip_bom(bytes)).ok().is_some_and(|value| value.get("format").and_then(|format| format.as_str()) == Some(FORMAT))
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(bytes)
}

fn slashes(value: &str) -> String {
    value.replace('\\', "/")
}

/// ドライブ付き（`C:/`）、UNC（`//server/share`）、`/` で始まるものを絶対パスとみなす。
fn is_absolute(value: &str) -> bool {
    let value = slashes(value);
    let bytes = value.as_bytes();
    value.starts_with('/') || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/')
}

fn fold(value: &str) -> String {
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value.to_owned()
    }
}

/// `..` と `.` を字句的に畳む。先頭のドライブ・UNC の共有名・ルートは残す。
fn clean(value: &str) -> String {
    let value = slashes(value);
    let (prefix, body) = if let Some(rest) = value.strip_prefix("//") {
        let mut parts = rest.splitn(3, '/');
        let server = parts.next().unwrap_or_default();
        let share = parts.next().unwrap_or_default();
        (format!("//{server}/{share}"), parts.next().unwrap_or_default().to_owned())
    } else if value.as_bytes().get(1) == Some(&b':') {
        (value[..2].to_owned(), value[2..].to_owned())
    } else {
        (String::new(), value.clone())
    };
    let rooted = !prefix.is_empty() || body.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for part in body.split('/') {
        match part {
            "" | "." => {}
            ".." if stack.last().is_some_and(|last| *last != "..") => {
                stack.pop();
            }
            ".." if rooted => {}
            other => stack.push(other),
        }
    }
    let joined = stack.join("/");
    match (prefix.is_empty(), rooted) {
        (false, _) => format!("{prefix}/{joined}"),
        (true, true) => format!("/{joined}"),
        (true, false) if joined.is_empty() => ".".to_owned(),
        (true, false) => joined,
    }
}

/// `value` が基準ルート `root` の下（またはそのもの）なら、`${PROJECT_ROOT}` からの表記にする。
pub fn to_portable(value: &str, root: &str) -> Option<String> {
    if !is_absolute(value) {
        return None;
    }
    let value = clean(value);
    let root = clean(root);
    let root = root.trim_end_matches('/');
    if fold(&value) == fold(root) {
        return Some(ROOT_TOKEN.to_owned());
    }
    let cut = root.len();
    let under = value.len() > cut && value.is_char_boundary(cut) && fold(&value[..cut]) == fold(root) && value.as_bytes()[cut] == b'/';
    under.then(|| format!("{ROOT_TOKEN}/{}", &value[cut + 1..]))
}

/// `${PROJECT_ROOT}` で始まる値を、基準ルート `root` の下のパスに戻す。それ以外はそのまま。
pub fn from_portable(value: &str, root: &str) -> String {
    match value.strip_prefix(ROOT_TOKEN) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') => native(&clean(&format!("{root}{rest}"))),
        _ => value.to_owned(),
    }
}

/// `canonicalize` が返す `\\?\C:\...` と `\\?\UNC\server\share\...` を、普通の表記に戻す。
fn without_verbatim(value: &str) -> String {
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        value.strip_prefix(r"\\?\").unwrap_or(value).to_owned()
    }
}

fn native(value: &str) -> String {
    if cfg!(windows) {
        value.replace('/', "\\")
    } else {
        value.to_owned()
    }
}

/// `from` ディレクトリから `to` への相対パス（`/` 区切り）。ドライブや共有が違えば表せない。
fn relative(from: &str, to: &str) -> Option<String> {
    let from = clean(from);
    let to = clean(to);
    let split = |value: &str| value.split('/').filter(|part| !part.is_empty()).map(str::to_owned).collect::<Vec<_>>();
    let (a, b) = (split(&from), split(&to));
    // ボリューム（ドライブ、または UNC のサーバーと共有）が同じでなければ、相対では表せない。
    let volume = if from.starts_with("//") { 2 } else { 1 };
    if from.starts_with("//") != to.starts_with("//") || a.len() < volume || b.len() < volume || (0..volume).any(|i| fold(&a[i]) != fold(&b[i])) {
        return None;
    }
    let common = a.iter().zip(&b).take_while(|(x, y)| fold(x) == fold(y)).count();
    let mut parts: Vec<String> = std::iter::repeat("..".to_owned()).take(a.len() - common).collect();
    parts.extend(b[common..].iter().cloned());
    Some(if parts.is_empty() { ".".to_owned() } else { parts.join("/") })
}

/// 書き換えの対象になる文字列を順にたどる。`program` は区切りを含むときだけ（PATH で引くものは除く）。
/// WSL で動くバリエーションのパスは Linux のパスなので、たどらない。
fn walk(project: &mut Project, assets: &mut [LibraryAsset], f: &mut dyn FnMut(&mut String)) {
    fn spec(spec: &mut ProgramSpec, f: &mut dyn FnMut(&mut String)) {
        if matches!(spec.target, ExecutionTarget::Wsl { .. }) {
            return;
        }
        if spec.program.contains(['/', '\\']) {
            f(&mut spec.program);
        }
        if let Some(cwd) = spec.cwd.as_mut() {
            f(cwd);
        }
        spec.args.iter_mut().for_each(&mut *f);
        spec.env.values_mut().for_each(&mut *f);
    }
    for asset in assets.iter_mut() {
        for variant in asset.variants.iter_mut() {
            let native = !matches!(variant.launch.target, ExecutionTarget::Wsl { .. });
            spec(&mut variant.launch, f);
            if native {
                variant.config_files.iter_mut().for_each(&mut *f);
                variant.pdu_entries.iter_mut().for_each(|entry| f(&mut entry.path));
                variant.extra_inputs.iter_mut().for_each(&mut *f);
            }
        }
    }
    project.core_env.values_mut().for_each(&mut *f);
    if let Some(directory) = project.source_directory.as_mut() {
        f(directory);
    }
    if let Some(controller) = project.core_controller.as_mut() {
        spec(&mut controller.command, f);
    }
    for member in project.members.iter_mut() {
        let wsl = assets.iter().find(|asset| asset.id == member.asset_id).and_then(|asset| asset.variant(&member.variant_id)).is_some_and(|variant| matches!(variant.launch.target, ExecutionTarget::Wsl { .. }));
        if !wsl {
            member.env_overrides.values_mut().for_each(&mut *f);
        }
    }
}

/// プロジェクトを書き出す形にする。`file` は書き出し先、`root` は基準ルート（絶対パス）。
/// 返す警告は、基準ルートの外に残った絶対パスや、値の一部に埋め込まれていて書き換えなかったパス。
pub fn export(project: &Project, library: &Library, file: &Path, root: &Path) -> Result<(Bundle, Vec<String>), String> {
    let root = root.display().to_string();
    if !is_absolute(&root) {
        return Err("基準ルートは絶対パスで指定してください。".to_owned());
    }
    let directory = file.parent().map(|parent| parent.display().to_string()).unwrap_or_default();
    let relative_root = relative(&directory, &root).ok_or_else(|| "基準ルートと書き出し先は、同じドライブに置いてください。".to_owned())?;
    let mut project = project.clone();
    for member in project.members.iter_mut() {
        member.accepted = None;
    }
    project.last_opened_at = None;
    let mut ids: Vec<&str> = project.members.iter().map(|member| member.asset_id.as_str()).collect();
    ids.sort();
    ids.dedup();
    let mut assets: Vec<LibraryAsset> = ids.into_iter().map(|id| library.get(id).cloned().ok_or_else(|| format!("アセット {id} がライブラリにありません。"))).collect::<Result<_, _>>()?;
    let mut warnings = Vec::new();
    let root_key = fold(&clean(&root));
    walk(&mut project, &mut assets, &mut |value: &mut String| {
        if value.contains(';') {
            // パスの並びは、先頭だけを書き換えると残りが元の場所を指したままになるので、書き換えない。
            if is_absolute(value) || fold(&slashes(value)).contains(&root_key) {
                warnings.push(format!("パスの並び（; 区切り）は書き換えていません（別の PC では見つからないかもしれません）: {value}"));
            }
        } else if let Some(portable) = to_portable(value, &root) {
            *value = portable;
        } else if is_absolute(value) {
            warnings.push(format!("基準ルートの外の絶対パスです（別の PC では見つからないかもしれません）: {value}"));
        } else if fold(&slashes(value)).contains(&root_key) {
            warnings.push(format!("値の一部に基準ルートの下のパスが含まれています（書き換えていません）: {value}"));
        }
    });
    warnings.sort();
    warnings.dedup();
    let bundle = Bundle { format: FORMAT.to_owned(), format_version: FORMAT_VERSION, exported_at: Utc::now(), root: relative_root, project, assets };
    Ok((bundle, warnings))
}

/// 書き出したものをファイルに書く（一時ファイルに書いてから置き換える）。
pub fn write(bundle: &Bundle, file: &Path) -> Result<(), String> {
    let content = serde_json::to_vec_pretty(bundle).map_err(|error| error.to_string())?;
    let temporary = PathBuf::from(format!("{}.tmp", file.display()));
    std::fs::write(&temporary, content).map_err(|error| format!("{} に書けません: {error}", temporary.display()))?;
    std::fs::rename(&temporary, file).map_err(|error| format!("{} を置き換えられません: {error}", file.display()))
}

/// 読み込む。`root_override` があれば、それを基準ルートにする。
pub fn load(file: &Path, root_override: Option<&Path>) -> Result<Loaded, String> {
    let bytes = std::fs::read(file).map_err(|error| format!("{} を読めません: {error}", file.display()))?;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let value: serde_json::Value = serde_json::from_slice(strip_bom(&bytes)).map_err(|error| format!("{} は JSON として読めません: {error}", file.display()))?;
    if value.get("format").and_then(|format| format.as_str()) != Some(FORMAT) {
        return Err(format!("{} はプロジェクトのファイル（{EXTENSION}）ではありません。", file.display()));
    }
    let version = value.get("format_version").and_then(|version| version.as_u64()).unwrap_or_default();
    if version != u64::from(FORMAT_VERSION) {
        return Err(format!("プロジェクトのファイルの形式 v{version} には対応していません（対応: v{FORMAT_VERSION}）。"));
    }
    let mut bundle: Bundle = serde_json::from_value(value).map_err(|error| format!("プロジェクトのファイルを読めません: {error}"))?;
    if bundle.project.schema_version != STORE_SCHEMA_VERSION {
        return Err(format!("プロジェクトの形式 v{} には対応していません。", bundle.project.schema_version));
    }
    let root = match root_override {
        Some(root) => clean(&root.display().to_string()),
        None => {
            let directory = file.parent().map(|parent| parent.display().to_string()).unwrap_or_default();
            let directory = std::fs::canonicalize(&directory).map(|path| without_verbatim(&path.display().to_string())).unwrap_or(directory);
            clean(&format!("{directory}/{}", bundle.root))
        }
    };
    let mut warnings = Vec::new();
    let Bundle { project, assets, .. } = &mut bundle;
    walk(project, assets, &mut |value: &mut String| *value = from_portable(value, &root));
    for asset in bundle.assets.iter() {
        asset.validate()?;
    }
    let library: Library = bundle.assets.iter().map(|asset| (asset.id.clone(), asset.clone())).collect();
    bundle.project.validate(&library)?;
    for member in bundle.project.members.iter_mut() {
        member.accepted = None;
    }
    if let Some(core) = &bundle.project.core_release {
        if !Path::new(&core.hako_cmd_path).is_file() {
            warnings.push(format!("Core（{}）の hako-cmd が見つからないので、Core の選択を外しました。Core 管理で選び直してください: {}", core.version, core.hako_cmd_path));
            bundle.project.core_release = None;
        }
    }
    for asset in &bundle.assets {
        for variant in &asset.variants {
            if matches!(variant.launch.target, ExecutionTarget::Wsl { .. }) {
                continue;
            }
            let cwd = variant.launch.cwd.as_deref().map(Path::new);
            if let Some(cwd) = cwd.filter(|cwd| !cwd.is_dir()) {
                warnings.push(format!("{}（{}）の作業ディレクトリが見つかりません: {}", asset.name, variant.label, cwd.display()));
            }
            for file in variant.config_files.iter().chain(variant.pdu_entries.iter().map(|entry| &entry.path)).chain(variant.extra_inputs.iter()) {
                let path = match cwd {
                    Some(cwd) if Path::new(file).is_relative() => cwd.join(file),
                    _ => PathBuf::from(file),
                };
                if !path.exists() {
                    warnings.push(format!("{}（{}）のファイルが見つかりません: {}", asset.name, variant.label, path.display()));
                }
            }
        }
    }
    warnings.sort();
    warnings.dedup();
    Ok(Loaded { bundle, sha256, root: native(&root), warnings })
}

/// 読み込んだアセットに付ける id。元の id から決めるので、同じファイルを読み込み直すと同じになり、
/// 内容が同じならライブラリのものを使い回す。読み込む側のファイル名に使えない id も持ち込まない。
pub fn imported_asset_id(original: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("hakoniwa-desktop-manager:hakoproj-asset:{original}").as_bytes()).to_string()
}

/// 読み込んだプロジェクトの出所。同じファイル（同じ書き出し元のプロジェクト）を読み込み直したとき、置き換える相手を探すのに使う。
pub fn origin(bundle: &Bundle) -> String {
    format!("hakoproj:{}", bundle.project.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::split_v1_workspace;
    use crate::resolve::tests::v1_demo;

    fn demo() -> (Project, Library) {
        let (assets, project) = split_v1_workspace(&v1_demo());
        (project, assets.into_iter().map(|asset| (asset.id.clone(), asset)).collect())
    }

    #[test]
    fn portable_paths_only_under_the_root() {
        assert_eq!(to_portable(r"C:\Demo\src\a.py", "C:/demo").as_deref(), Some("${PROJECT_ROOT}/src/a.py"));
        assert_eq!(to_portable("C:/demo", r"C:\Demo\").as_deref(), Some("${PROJECT_ROOT}"));
        assert_eq!(to_portable("C:/demolition/a.py", "C:/demo"), None, "名前の前方一致は下ではない");
        assert_eq!(to_portable("C:/Python/python.exe", "C:/demo"), None);
        assert_eq!(to_portable("src/a.py", "C:/demo"), None, "相対パスはそのまま");
        assert_eq!(to_portable("--config=C:/demo/a.json", "C:/demo"), None);
        assert_eq!(to_portable("//server/share/demo/x", "//server/share/demo").as_deref(), Some("${PROJECT_ROOT}/x"));
        assert_eq!(from_portable("${PROJECT_ROOT}/src/../src/a.py", "D:/elsewhere"), native("D:/elsewhere/src/a.py"));
        assert_eq!(from_portable("${PROJECT_ROOTX}/a", "D:/x"), "${PROJECT_ROOTX}/a");
        assert_eq!(from_portable("python", "D:/x"), "python");
    }

    #[test]
    fn relative_roots() {
        assert_eq!(relative("C:/repo/configs/demo/launcher", "C:/repo").as_deref(), Some("../../.."));
        assert_eq!(relative("C:/repo", "C:/repo").as_deref(), Some("."));
        assert_eq!(relative("C:/a", "D:/a"), None);
        assert_eq!(relative("//server/share/out", "//server/share/root").as_deref(), Some("../root"));
        assert_eq!(relative("//server/shareA/out", "//server/shareB/root"), None, "共有が違えば表せない");
        assert_eq!(without_verbatim(r"\\?\UNC\server\share\out"), r"\\server\share\out");
        assert_eq!(without_verbatim(r"\\?\C:\repo"), r"C:\repo");
        assert_eq!(clean("C:/a/../../b"), "C:/b", "ルートより上には出ない");
    }

    /// 書き出して、別の場所に移したファイルを読み込むと、パスが移した先の基準ルートの下になる。
    #[test]
    fn round_trip_moves_paths_with_the_file() {
        let (mut project, library) = demo();
        project.members[0].accepted = Some(crate::model::Acceptance { revision: "r".to_owned(), manifest: Vec::new(), accepted_at: Utc::now(), had_pdu_entries: false, channels: Vec::new() });
        let dir = std::env::temp_dir().join(format!("hdm-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out = dir.join("launcher");
        std::fs::create_dir_all(&out).unwrap();
        let file = out.join("demo.hakoproj.json");
        // v1_demo のパスは C:/demo の下、python は C:/miniconda3 にある。
        let (bundle, warnings) = export(&project, &library, &file, Path::new("C:/demo")).unwrap();
        assert!(bundle.project.members.iter().all(|member| member.accepted.is_none()), "承認は書き出さない");
        assert!(bundle.assets.iter().all(|asset| asset.variants[0].launch.cwd.as_deref() == Some("${PROJECT_ROOT}")));
        assert!(warnings.iter().any(|warning| warning.contains("miniconda3")), "基準の外の python は警告: {warnings:?}");
        write(&bundle, &file).unwrap();
        assert!(is_bundle(&std::fs::read(&file).unwrap()));

        let elsewhere = dir.join("moved");
        let loaded = load(&file, Some(&elsewhere)).unwrap();
        let cwd = loaded.bundle.assets[0].variants[0].launch.cwd.clone().unwrap();
        assert_eq!(fold(&slashes(&cwd)), fold(&clean(&elsewhere.display().to_string())));
        assert!(loaded.bundle.project.members.iter().all(|member| member.accepted.is_none()));
        assert!(loaded.warnings.iter().any(|warning| warning.contains("作業ディレクトリ")), "移した先にディレクトリがない: {:?}", loaded.warnings);
        // 基準ルートを指定しなければ、ファイルの位置と記録した相対位置から決める。
        let default = load(&file, None).unwrap();
        assert_eq!(bundle.root, relative(&out.display().to_string(), "C:/demo").unwrap());
        assert!(default.bundle.assets[0].variants[0].launch.cwd.is_some());
    }

    /// 取り込み: 承認は付かない。読み込み直すと同じプロジェクトを置き換え、アセットは使い回す。
    /// ライブラリの同じ id のアセットを書き換えていても、それは変えずに別のアセットとして足す。
    #[test]
    fn importing_reuses_assets_and_never_overwrites_the_library() {
        let (project, library) = demo();
        let dir = std::env::temp_dir().join(format!("hdm-bundle-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let file = dir.join("demo.hakoproj.json");
        let (bundle, _) = export(&project, &library, &file, Path::new("C:/demo")).unwrap();
        write(&bundle, &file).unwrap();
        let loaded = load(&file, Some(Path::new("C:/demo"))).unwrap();

        let mut store = crate::store::Store::open(&dir.join("data"));
        let before = store.projects.len();
        let first = store.import_bundle(&loaded, true, false).unwrap();
        assert!(!first.replaced);
        assert!(store.projects[&first.project_id].members.iter().all(|member| member.accepted.is_none()), "承認は付かない");
        let count = store.library.len();
        assert!(loaded.bundle.assets.iter().all(|asset| store.reuses(asset)));
        let second = store.import_bundle(&loaded, true, false).unwrap();
        assert!(second.replaced && second.project_id == first.project_id);
        assert_eq!(store.library.len(), count, "同じ内容なら使い回す");
        assert_eq!(store.projects.len(), before + 1);

        // 同じ id のライブラリのアセットを書き換えておくと、取り込みはそれを変えずに別のアセットを足す。
        let id = imported_asset_id(&loaded.bundle.assets[0].id);
        let mut edited = store.library[&id].clone();
        edited.description = "edited here".to_owned();
        store.save_asset(&edited).unwrap();
        store.library.insert(id.clone(), edited);
        let third = store.import_bundle(&loaded, false, true).unwrap();
        assert_ne!(third.project_id, first.project_id, "置き換えなければ新しいプロジェクト");
        assert_eq!(store.library[&id].description, "edited here");
        let user = loaded.bundle.project.members.iter().find(|member| member.asset_id == loaded.bundle.assets[0].id).unwrap().id.clone();
        assert_ne!(store.projects[&third.project_id].member(&user).unwrap().asset_id, id, "書き換えたアセットは使わず、複製に付け替える");
        assert!(store.projects[&third.project_id].members.iter().all(|member| member.accepted.is_some()), "accept のときだけ承認する");
        assert!(store.projects[&third.project_id].origin.is_none(), "複製は出所を持たない");
    }

    /// 生成スクリプトが書いた `.hakoproj.json` を読み込むと、同じスクリプトが書いた v1 と同じ構成になり、
    /// 承認すれば一括起動を止める理由がない。`HDM_GIMBAL_BUNDLE` と `HDM_GIMBAL_WORKSPACE` を渡したときだけ実行する。
    #[test]
    fn gimbal_drone_demo_bundle_matches_the_v1_workspace() {
        let (Ok(bundle_path), Ok(v1_path)) = (std::env::var("HDM_GIMBAL_BUNDLE"), std::env::var("HDM_GIMBAL_WORKSPACE")) else { return };
        let loaded = load(Path::new(&bundle_path), None).unwrap();
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let mut project = loaded.bundle.project.clone();
        let library: Library = loaded.bundle.assets.iter().map(|asset| (asset.id.clone(), asset.clone())).collect();
        let resolved = crate::resolve::resolve(&project, &library).unwrap();
        let v1: crate::types::Workspace = serde_json::from_slice(&std::fs::read(&v1_path).unwrap()).unwrap();
        let key = |path: &Option<String>| path.as_deref().map(|path| fold(&clean(path)));
        for (a, b) in resolved.assets.iter().zip(&v1.assets) {
            assert_eq!(a.id, b.id);
            assert_eq!(key(&a.command.cwd), key(&b.command.cwd), "{}", a.id);
            assert_eq!(a.command.args, b.command.args, "{}", a.id);
            assert_eq!(a.config_files, b.config_files, "{}", a.id);
            assert_eq!(a.depends_on, b.depends_on, "{}", a.id);
            assert_eq!(a.optional_depends_on, b.optional_depends_on, "{}", a.id);
            assert_eq!(a.enabled, b.enabled, "{}", a.id);
            let env = |env: &std::collections::BTreeMap<String, String>| env.iter().map(|(k, v)| (k.clone(), fold(&clean(v)))).collect::<Vec<_>>();
            assert_eq!(env(&a.command.env), env(&b.command.env), "{}", a.id);
        }
        assert_eq!(resolved.assets.len(), v1.assets.len());
        assert!(project.core_release.is_some());
        crate::check::accept_all(&mut project, &library).unwrap();
        let check = crate::check::check_project(&project, &library).unwrap();
        assert!(check.blocking_reasons().is_empty(), "{:?}", check.blocking_reasons());
    }

    /// `HDM_EXTRA_BUNDLES`（`;` 区切り）に渡した `.hakoproj.json` が、警告なしに読み込めて解決でき、
    /// 承認すれば一括起動を止める理由がない。渡したときだけ実行する。
    #[test]
    fn extra_bundles_load_cleanly() {
        let Ok(files) = std::env::var("HDM_EXTRA_BUNDLES") else { return };
        for file in files.split(';').filter(|file| !file.is_empty()) {
            let loaded = load(Path::new(file), None).unwrap_or_else(|error| panic!("{file}: {error}"));
            assert!(loaded.warnings.is_empty(), "{file}: {:?}", loaded.warnings);
            let mut project = loaded.bundle.project.clone();
            let library: Library = loaded.bundle.assets.iter().map(|asset| (asset.id.clone(), asset.clone())).collect();
            crate::resolve::resolve(&project, &library).unwrap();
            crate::check::accept_all(&mut project, &library).unwrap();
            let check = crate::check::check_project(&project, &library).unwrap();
            assert!(check.blocking_reasons().is_empty(), "{file}: {:?}", check.blocking_reasons());
        }
    }

    /// パスの並び（PATH など）は書き換えず、警告に出す。WSL のメンバーの環境変数の上書きは書き換えない。
    #[test]
    fn path_lists_and_wsl_overrides_are_left_alone() {
        let (mut project, mut library) = demo();
        let first = project.members[0].asset_id.clone();
        library.get_mut(&first).unwrap().variants[0].launch.env.insert("PATH".to_owned(), "C:/demo/bin;C:/demo/tools".to_owned());
        let second = project.members[1].clone();
        library.get_mut(&second.asset_id).unwrap().variants[0].launch.target = ExecutionTarget::Wsl { distribution: "Ubuntu".to_owned() };
        project.members[1].env_overrides.insert("DATA".to_owned(), "C:/demo/data".to_owned());
        let file = std::env::temp_dir().join("hdm-bundle-lists").join("x.hakoproj.json");
        let (bundle, warnings) = export(&project, &library, &file, Path::new("C:/demo")).unwrap();
        let exported = bundle.assets.iter().find(|asset| asset.id == first).unwrap();
        assert_eq!(exported.variants[0].launch.env["PATH"], "C:/demo/bin;C:/demo/tools");
        assert!(warnings.iter().any(|warning| warning.contains("パスの並び")), "{warnings:?}");
        assert_eq!(bundle.project.members[1].env_overrides["DATA"], "C:/demo/data", "WSL のメンバーは書き換えない");
    }

    /// ライブラリに残してある（どのプロジェクトも使っていない）アセットと同じ id で内容の違うものを
    /// 取り込んでも、残してあるアセットは消えない。
    #[test]
    fn importing_keeps_an_unused_library_asset_with_the_same_id() {
        let (project, library) = demo();
        let dir = std::env::temp_dir().join(format!("hdm-bundle-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let file = dir.join("demo.hakoproj.json");
        let (bundle, _) = export(&project, &library, &file, Path::new("C:/demo")).unwrap();
        write(&bundle, &file).unwrap();
        let loaded = load(&file, Some(Path::new("C:/demo"))).unwrap();
        let mut store = crate::store::Store::open(&dir.join("data"));
        let id = imported_asset_id(&loaded.bundle.assets[0].id);
        let mut kept = loaded.bundle.assets[0].clone();
        kept.id = id.clone();
        kept.description = "kept in the library".to_owned();
        store.save_asset(&kept).unwrap();
        store.library.insert(id.clone(), kept);
        store.import_bundle(&loaded, true, false).unwrap();
        assert_eq!(store.library[&id].description, "kept in the library");
        assert!(dir.join("data").join("library").join("assets").join(format!("{id}.json")).is_file());
    }

    #[test]
    fn unknown_formats_are_refused() {
        let dir = std::env::temp_dir().join(format!("hdm-bundle-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.hakoproj.json");
        std::fs::write(&file, br#"{"format":"something-else"}"#).unwrap();
        assert!(load(&file, None).is_err());
        std::fs::write(&file, br#"{"format":"hakoniwa-desktop-manager/project","format_version":2}"#).unwrap();
        assert!(load(&file, None).unwrap_err().contains("v2"));
    }
}
