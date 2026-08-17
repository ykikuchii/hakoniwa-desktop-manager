use crate::types::{
    ArchiveFormat, CoreArtifact, CoreCatalog, CoreInstallResult, CpuArchitecture,
    HostPlatform, InstalledCoreSelection,
};
use chrono::Utc;
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::{fs, io::Cursor, path::{Component, Path}, time::Duration};
use thiserror::Error;
use zip::ZipArchive;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("カタログを読み込めませんでした: {0}")]
    Catalog(#[from] serde_json::Error),
    #[error("ファイル操作に失敗しました: {0}")]
    Io(#[from] std::io::Error),
    #[error("ダウンロードに失敗しました: {0}")]
    Http(#[from] reqwest::Error),
    #[error("指定したCoreバージョンは承認済みカタログにありません: {0}")]
    VersionNotApproved(String),
    #[error("この環境に対応する承認済みアーティファクトがありません（{0}/{1}）。")]
    PlatformNotApproved(String, String),
    #[error("アーティファクトURLはHTTPSである必要があります。")]
    InsecureUrl,
    #[error("SHA-256値が不正です。")]
    InvalidHash,
    #[error("ダウンロードしたアーティファクトのSHA-256が一致しません。")]
    HashMismatch,
    #[error("アーカイブ展開中に危険なパスが検出されました。")]
    UnsafeArchivePath,
    #[error("展開後にhako-cmdが見つかりません: {0}")]
    HakoCmdMissing(String),
    #[error("現在のCoreを置換できません。実行中のプロセスを停止してから再試行してください。")]
    InstallBusy,
}

pub fn load_catalog(path: &Path) -> Result<CoreCatalog, CoreError> {
    let catalog: CoreCatalog = serde_json::from_slice(&fs::read(path)?)?;
    if catalog.schema_version != crate::types::CATALOG_SCHEMA_VERSION || catalog.component != "hakoniwa-core-pro" {
        return Err(CoreError::VersionNotApproved("カタログの形式またはコンポーネント識別子が正しくありません。".to_owned()));
    }
    Ok(catalog)
}

pub fn install_core(
    catalog: &CoreCatalog,
    version: &str,
    app_data_dir: &Path,
) -> Result<CoreInstallResult, CoreError> {
    let release = catalog.releases.iter().find(|release| release.version == version)
        .ok_or_else(|| CoreError::VersionNotApproved(version.to_owned()))?;
    let platform = HostPlatform::current();
    let architecture = CpuArchitecture::current();
    let artifact = release.artifacts.iter().find(|artifact| artifact.platform == platform && artifact.architecture == architecture)
        .ok_or_else(|| CoreError::PlatformNotApproved(platform.as_str().to_owned(), architecture.as_str().to_owned()))?;
    validate_artifact(artifact)?;

    let payload = download(artifact)?;
    let actual_hash = hex::encode(Sha256::digest(&payload));
    if !actual_hash.eq_ignore_ascii_case(&artifact.sha256) {
        return Err(CoreError::HashMismatch);
    }

    let component_root = app_data_dir.join("core").join("hakoniwa-core-pro");
    fs::create_dir_all(&component_root)?;
    let final_dir = component_root.join(&release.version);
    let staged_dir = component_root.join(format!(".{}.staging", release.version));
    if staged_dir.exists() {
        fs::remove_dir_all(&staged_dir)?;
    }
    fs::create_dir_all(&staged_dir)?;
    unpack(&payload, artifact, &staged_dir)?;
    let install_root = artifact.install_root.as_deref().map(|value| staged_dir.join(value)).unwrap_or_else(|| staged_dir.clone());
    let hako_cmd = install_root.join(&artifact.hako_cmd_relative_path);
    if !hako_cmd.is_file() {
        let _ = fs::remove_dir_all(&staged_dir);
        return Err(CoreError::HakoCmdMissing(hako_cmd.display().to_string()));
    }

    if final_dir.exists() {
        fs::remove_dir_all(&final_dir).map_err(|_| CoreError::InstallBusy)?;
    }
    fs::rename(&staged_dir, &final_dir)?;
    let final_install_root = artifact.install_root.as_deref().map(|value| final_dir.join(value)).unwrap_or_else(|| final_dir.clone());
    let final_hako_cmd = final_install_root.join(&artifact.hako_cmd_relative_path);
    let selection = InstalledCoreSelection {
        version: release.version.clone(),
        install_directory: final_install_root.display().to_string(),
        hako_cmd_path: final_hako_cmd.display().to_string(),
        verified_sha256: actual_hash.clone(),
        installed_at: Utc::now(),
    };
    Ok(CoreInstallResult { selection, artifact_url: artifact.url.clone(), artifact_sha256: actual_hash })
}

fn validate_artifact(artifact: &CoreArtifact) -> Result<(), CoreError> {
    if !artifact.url.starts_with("https://") {
        return Err(CoreError::InsecureUrl);
    }
    if artifact.sha256.len() != 64 || !artifact.sha256.bytes().all(|character| character.is_ascii_hexdigit()) {
        return Err(CoreError::InvalidHash);
    }
    if artifact.hako_cmd_relative_path.is_empty() || is_unsafe_path(Path::new(&artifact.hako_cmd_relative_path)) {
        return Err(CoreError::UnsafeArchivePath);
    }
    Ok(())
}

fn download(artifact: &CoreArtifact) -> Result<Vec<u8>, CoreError> {
    let client = Client::builder().timeout(Duration::from_secs(120)).user_agent("HakoniwaDesktopManager/0.1").build()?;
    let response = client.get(&artifact.url).send()?.error_for_status()?;
    Ok(response.bytes()?.to_vec())
}

fn unpack(payload: &[u8], artifact: &CoreArtifact, destination: &Path) -> Result<(), CoreError> {
    match artifact.archive_format {
        ArchiveFormat::File => {
            let path = destination.join(&artifact.hako_cmd_relative_path);
            if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
            fs::write(path, payload)?;
            Ok(())
        }
        ArchiveFormat::Zip => {
            let mut archive = ZipArchive::new(Cursor::new(payload)).map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            for index in 0..archive.len() {
                let mut entry = archive.by_index(index).map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
                let entry_path = Path::new(entry.name());
                if is_unsafe_path(entry_path) { return Err(CoreError::UnsafeArchivePath); }
                let output_path = destination.join(entry_path);
                if entry.is_dir() {
                    fs::create_dir_all(&output_path)?;
                    continue;
                }
                if let Some(parent) = output_path.parent() { fs::create_dir_all(parent)?; }
                let mut output = fs::File::create(&output_path)?;
                std::io::copy(&mut entry, &mut output)?;
            }
            Ok(())
        }
    }
}

fn is_unsafe_path(path: &Path) -> bool {
    path.is_absolute() || path.components().any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ArchiveFormat, ArtifactProvenance, CpuArchitecture, HostPlatform};

    #[test]
    fn rejects_non_https_artifact() {
        let artifact = CoreArtifact {
            platform: HostPlatform::Linux,
            architecture: CpuArchitecture::X64,
            url: "http://example.test/core.zip".to_owned(),
            sha256: "a".repeat(64),
            archive_format: ArchiveFormat::Zip,
            hako_cmd_relative_path: "bin/hako-cmd".to_owned(),
            install_root: None,
            provenance: ArtifactProvenance::default(),
        };
        assert!(matches!(validate_artifact(&artifact), Err(CoreError::InsecureUrl)));
    }

    #[test]
    fn rejects_parent_directory() {
        assert!(is_unsafe_path(Path::new("../evil")));
    }
}
