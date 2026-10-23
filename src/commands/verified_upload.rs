use crate::{
    config::load_config,
    oss::{OssClient, client::VerifiedReceipt},
};
use anyhow::{Result, ensure};
use std::io::Write;
use std::path::{Path, PathBuf};

fn reserve_report(path: &Path) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(b"{\"protocol\":\"nuwax-verified-upload-v1\",\"files\":[]}")?;
    file.sync_all()?;
    Ok(())
}

fn save_report(path: &Path, receipts: &[VerifiedReceipt]) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(
        &serde_json::json!({"protocol":"nuwax-verified-upload-v1", "files":receipts}),
    )?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let tmp = path.with_extension(format!("json.{}.{nonce}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, path)?;
    Ok(())
}

pub async fn upload(
    paths: &[PathBuf],
    remote: &str,
    report: &Path,
    bucket: &str,
    endpoint: &str,
) -> Result<()> {
    ensure!(
        !remote.is_empty()
            && !remote.starts_with('/')
            && !remote.ends_with('/')
            && remote
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != ".."),
        "invalid strict remote prefix"
    );
    let mut names = std::collections::HashSet::new();
    for path in paths {
        ensure!(
            path.is_file() && !path.is_symlink(),
            "strict upload accepts explicit regular files only"
        );
        ensure!(
            names.insert(path.file_name().unwrap()),
            "duplicate upload basename"
        );
    }
    ensure!(
        !report.exists(),
        "report already exists; preserve the original receipt"
    );
    let client = OssClient::new_verified(&load_config()?)?;
    client.check_release_bucket(bucket, endpoint).await?;
    reserve_report(report)?;
    let mut receipts: Vec<VerifiedReceipt> = vec![];
    for path in paths {
        let key = format!(
            "{}/{}",
            remote,
            path.file_name()
                .unwrap()
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid filename"))?
        );
        eprintln!("strict upload {} -> {}/{}", path.display(), bucket, key);
        receipts.push(client.upload_verified(path, &key).await?);
        save_report(report, &receipts)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_reservation_never_overwrites_an_existing_receipt() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("oss-report-{}-{nonce}.json", std::process::id()));
        reserve_report(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(reserve_report(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        save_report(&path, &[]).unwrap();
        assert!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).is_ok()
        );
        std::fs::remove_file(path).unwrap();
    }
}
