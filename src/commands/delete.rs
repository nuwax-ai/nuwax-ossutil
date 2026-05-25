use anyhow::Result;

use crate::config::load_config;
use crate::oss::OssClient;

pub async fn delete_file(remote_path: &str) -> Result<()> {
    let config = load_config()?;
    let client = OssClient::new(&config)?;

    println!("🗑️  删除文件: {}/{}", config.bucket_name, remote_path);

    client.delete(remote_path).await?;

    println!("✅ 删除成功!");

    Ok(())
}
