use anyhow::Result;
use chrono::Utc;
use futures::future::join_all;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::common::{expand_paths, format_file_size, UploadResult};
use crate::config::load_config;
use crate::oss::client::ProgressCallback;
use crate::oss::{OssClient, guess_content_type};

const PROGRESS_TEMPLATE: &str =
    "{msg:<15} [{elapsed_precise}] [{bar:20.cyan/blue}] {bytes}/{total_bytes} ({percent}%) {binary_bytes_per_sec}";

fn new_progress_style() -> ProgressStyle {
    ProgressStyle::with_template(PROGRESS_TEMPLATE)
        .unwrap()
        .progress_chars("██░")
        .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ ")
}

pub async fn upload_docker_files(files: &[PathBuf]) -> Result<()> {
    // Expand directories into file lists (always skip sub-directories)
    let files = expand_paths(files, true)?;

    if files.is_empty() {
        return Err(anyhow::anyhow!("没有可上传的文件"));
    }

    let config = load_config()?;

    // Fail Fast: validate all files exist and are regular files
    for file in &files {
        if !file.exists() {
            return Err(anyhow::anyhow!("文件不存在: {}", file.display()));
        }
        if !file.is_file() {
            return Err(anyhow::anyhow!("不是文件: {}", file.display()));
        }
    }

    let client = OssClient::new(&config)?;

    // Generate timestamp directory
    let timestamp_dir = Utc::now().format("%Y%m%d%H%M%S").to_string();
    let total = files.len();

    println!("📤 开始上传 {} 个文件到 docker/{}/", total, timestamp_dir);
    println!();

    let mp = MultiProgress::new();
    let mut upload_futures = Vec::new();

    for file in &files {
        let filename = file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let remote_path = format!("docker/{}/{}", timestamp_dir, filename);
        let content_type = guess_content_type(&filename);
        let local_path = file.to_string_lossy().to_string();
        let client = client.clone();

        let file_size = tokio::fs::metadata(file)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
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
                .upload_file_smart(&local_path, &remote_path, &content_type, Some(on_progress))
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

    // Execute all uploads concurrently
    let results = join_all(upload_futures).await;

    // Clear all progress bars
    mp.clear()?;

    // Print results
    let mut success_count = 0;
    for (i, r) in results.iter().enumerate() {
        let size_str = format_file_size(r.file_size);
        let upload_type = if r.is_multipart {
            " (分片上传)"
        } else {
            ""
        };

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
        Err(anyhow::anyhow!("所有文件上传失败"))
    } else if success_count < total {
        Err(anyhow::anyhow!(
            "部分文件上传失败: {}/{}",
            total - success_count,
            total
        ))
    } else {
        Ok(())
    }
}
