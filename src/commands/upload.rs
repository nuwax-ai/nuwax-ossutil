use anyhow::{anyhow, Result};
use futures::future::join_all;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::common::{expand_paths, format_file_size, UploadResult};
use crate::config::load_config;
use crate::oss::client::ProgressCallback;
use crate::oss::{guess_content_type, validate_file_path, OssClient};

const PROGRESS_TEMPLATE: &str =
    "{msg:<15} [{elapsed_precise}] [{bar:20.cyan/blue}] {bytes}/{total_bytes} ({percent}%) {binary_bytes_per_sec}";

fn new_progress_style() -> ProgressStyle {
    ProgressStyle::with_template(PROGRESS_TEMPLATE)
        .unwrap()
        .progress_chars("██░")
        .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ ")
}

/// Upload one or more files to a remote directory.
///
/// - `remote_dir` is always treated as a directory path (trailing `/` auto-appended).
/// - File names are derived from the local file name.
/// - `rename` only works for single-file uploads.
/// - `skip_dir`: if true, skip directories silently; if false, error on directories.
/// - Directories passed as input are auto-expanded to their direct children.
pub async fn upload_files(files: &[PathBuf], remote_dir: &str, rename: Option<&str>, skip_dir: bool) -> Result<()> {
    let files = expand_paths(files, skip_dir)?;

    if files.is_empty() {
        return Err(anyhow!("没有可上传的文件"));
    }

    if rename.is_some() && files.len() > 1 {
        return Err(anyhow!("--name 参数仅在上传单个文件时有效"));
    }

    if files.len() == 1 {
        let file_path = files[0].to_str().ok_or_else(|| anyhow!("无效的文件路径"))?;
        if !validate_file_path(file_path) {
            return Err(anyhow!("文件不存在或不是普通文件: {}", file_path));
        }
        upload_single_file(file_path, remote_dir, rename).await
    } else {
        upload_multiple_files(&files, remote_dir).await
    }
}

/// Normalize remote directory path: ensure it ends with `/`
fn normalize_remote_dir(remote: &str) -> String {
    if remote.ends_with('/') {
        remote.to_string()
    } else {
        format!("{}/", remote)
    }
}

/// Upload a single file to OSS with progress bar.
async fn upload_single_file(file_path: &str, remote_dir: &str, rename: Option<&str>) -> Result<()> {
    let config = load_config()?;
    let client = OssClient::new(&config)?;

    let dir = normalize_remote_dir(remote_dir);
    let filename = rename
        .map(String::from)
        .unwrap_or_else(|| {
            PathBuf::from(file_path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });
    let full_remote = format!("{}{}", dir, filename);

    let file_size = tokio::fs::metadata(file_path).await.map(|m| m.len()).unwrap_or(0);

    println!("📤 开始上传: {} -> {}/{}", file_path, config.bucket_name, full_remote);
    println!("   文件大小: {} bytes", file_size);

    // Create progress bar
    let pb = ProgressBar::new(file_size);
    pb.set_style(new_progress_style());
    pb.set_message(filename.clone());
    pb.enable_steady_tick(Duration::from_millis(200));

    let pb_clone = pb.clone();
    let on_progress: ProgressCallback = Arc::new(move |bytes_transferred| {
        pb_clone.inc(bytes_transferred);
    });

    let content_type = guess_content_type(&full_remote);
    let url = client.upload_file_smart(file_path, &full_remote, &content_type, Some(on_progress)).await?;

    pb.finish_and_clear();

    println!("✅ 上传成功!");
    println!("   下载地址: {}", url);
    Ok(())
}

/// Upload multiple files to OSS under a directory with per-file progress bars.
/// Note: directories should already be expanded by expand_paths before calling this.
async fn upload_multiple_files(files: &[PathBuf], remote_dir: &str) -> Result<()> {
    let config = load_config()?;
    let client = OssClient::new(&config)?;

    // Fail Fast: validate all files exist and are regular files
    for file in files {
        if !file.exists() {
            return Err(anyhow!("文件不存在: {}", file.display()));
        }
        if !file.is_file() {
            return Err(anyhow!("不是普通文件: {}", file.display()));
        }
    }

    let dir = normalize_remote_dir(remote_dir);
    let total = files.len();

    println!("📤 开始上传 {} 个文件到 {}/{}", total, config.bucket_name, dir);
    println!();

    let mp = MultiProgress::new();
    let mut upload_futures = Vec::new();

    for file in files {
        let filename = file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let full_remote = format!("{}{}", dir, filename);
        let content_type = guess_content_type(&filename);
        let local_path = file.to_string_lossy().to_string();
        let client = client.clone();

        let file_size = tokio::fs::metadata(file).await.map(|m| m.len()).unwrap_or(0);
        let is_multipart = file_size >= 5 * 1024 * 1024;

        // Create per-file progress bar
        let pb = mp.add(ProgressBar::new(file_size));
        pb.set_style(new_progress_style());
        pb.set_message(filename.clone());
        pb.enable_steady_tick(Duration::from_millis(200));

        let pb_clone = pb.clone();
        let on_progress: ProgressCallback = Arc::new(move |bytes_transferred| {
            pb_clone.inc(bytes_transferred);
        });

        upload_futures.push(async move {
            let result = client
                .upload_file_smart(&local_path, &full_remote, &content_type, Some(on_progress))
                .await;

            let (download_url, error) = match result {
                Ok(url) => (Some(url), None),
                Err(e) => (None, Some(e.to_string())),
            };

            UploadResult {
                filename,
                file_size,
                is_multipart,
                download_url,
                error,
            }
        });
    }

    let results = join_all(upload_futures).await;

    // Clear all progress bars
    mp.clear()?;

    // Print results
    let mut success_count = 0;
    for (i, r) in results.iter().enumerate() {
        let size_str = format_file_size(r.file_size);
        let upload_type = if r.is_multipart { " (分片上传)" } else { "" };

        if r.download_url.is_some() {
            success_count += 1;
            println!(
                "  [{}/{}] {} ({}) ... ✅ 成功{}",
                i + 1,
                total,
                r.filename,
                size_str,
                upload_type
            );
            if let Some(url) = &r.download_url {
                println!("         下载: {}", url);
            }
        } else {
            println!(
                "  [{}/{}] {} ({}) ... ❌ 失败{}",
                i + 1,
                total,
                r.filename,
                size_str,
                upload_type
            );
            if let Some(err) = &r.error {
                println!("         错误: {}", err);
            }
        }
    }

    println!();
    println!("📊 上传完成: 成功 {}/{}", success_count, total);

    if success_count == 0 {
        Err(anyhow!("所有文件上传失败"))
    } else if success_count < total {
        Err(anyhow!("部分文件上传失败: {}/{}", total - success_count, total))
    } else {
        Ok(())
    }
}
