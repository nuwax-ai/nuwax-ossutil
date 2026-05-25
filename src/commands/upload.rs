use anyhow::Result;

use crate::config::load_config;
use crate::oss::{guess_content_type, validate_file_path};

pub async fn upload_file(file_path: &str, remote_path: &str) -> Result<()> {
    let config = load_config()?;

    // Fail Fast: validate inputs before creating client
    if !validate_file_path(file_path) {
        return Err(anyhow::anyhow!("文件不存在或不是普通文件: {}", file_path));
    }

    let client = crate::oss::OssClient::new(&config)?;

    let file_size = tokio::fs::metadata(file_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    println!(
        "📤 开始上传: {} -> {}/{}",
        file_path, config.bucket_name, remote_path
    );
    println!("   文件大小: {} bytes", file_size);

    let content_type = guess_content_type(remote_path);
    // Use upload_file_smart for proper semaphore control and multipart support
    let url = client
        .upload_file_smart(file_path, remote_path, &content_type)
        .await?;

    println!("✅ 上传成功!");
    println!("   下载地址: {}", url);

    Ok(())
}
