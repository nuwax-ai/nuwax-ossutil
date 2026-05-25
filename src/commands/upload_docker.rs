use anyhow::Result;
use chrono::Utc;
use futures::future::join_all;
use std::path::PathBuf;

use crate::config::load_config;
use crate::oss::{OssClient, guess_content_type};

struct UploadResult {
    filename: String,
    success: bool,
    download_url: Option<String>,
    file_size: u64,
    error: Option<String>,
    is_multipart: bool,
}

pub async fn upload_docker_files(files: &[PathBuf]) -> Result<()> {
    let config = load_config()?;

    // Validate all files exist (Fail Fast)
    for file in files {
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

    // Build upload tasks
    let mut upload_futures = Vec::new();

    for file in files {
        let filename = file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let remote_path = format!("docker/{}/{}", timestamp_dir, filename);
        let content_type = guess_content_type(&filename);
        let local_path = file.to_string_lossy().to_string();
        let client = client.clone();

        upload_futures.push(async move {
            let file_size = tokio::fs::metadata(&local_path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            let is_multipart = file_size >= 5 * 1024 * 1024;

            match client
                .upload_file_smart(&local_path, &remote_path, &content_type)
                .await
            {
                Ok(url) => UploadResult {
                    filename,
                    success: true,
                    download_url: Some(url),
                    file_size,
                    error: None,
                    is_multipart,
                },
                Err(e) => UploadResult {
                    filename,
                    success: false,
                    download_url: None,
                    file_size,
                    error: Some(e.to_string()),
                    is_multipart,
                },
            }
        });
    }

    // Execute all uploads concurrently
    let results = join_all(upload_futures).await;

    // Print results
    let mut success_count = 0;
    for (i, result) in results.iter().enumerate() {
        let size_str = format_file_size(result.file_size);
        let upload_type = if result.is_multipart {
            " (分片上传)"
        } else {
            ""
        };

        if result.success {
            success_count += 1;
            println!(
                "  [{}/{}] {} ({}) ... ✅ 成功{}",
                i + 1,
                total,
                result.filename,
                size_str,
                upload_type
            );
            if let Some(url) = &result.download_url {
                println!("         下载: {}", url);
            }
        } else {
            println!(
                "  [{}/{}] {} ({}) ... ❌ 失败{}",
                i + 1,
                total,
                result.filename,
                size_str,
                upload_type
            );
            if let Some(err) = &result.error {
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

fn format_file_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
