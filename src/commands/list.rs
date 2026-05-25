use anyhow::Result;

use crate::config::load_config;
use crate::oss::OssClient;

pub async fn list_files(prefix: &str) -> Result<()> {
    let config = load_config()?;
    let client = OssClient::new(&config)?;

    println!("📁 列出文件: {}/{}", config.bucket_name, prefix);

    let files = client.list(prefix).await?;

    if files.is_empty() {
        println!("   (空)");
    } else {
        for file in &files {
            println!("   - {}", file);
        }
    }

    println!("\n共 {} 个文件", files.len());

    Ok(())
}
