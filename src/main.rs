use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod commands;
mod config;
mod oss;

#[derive(Parser)]
#[command(name = "nuwax-ossutil", version, about = "阿里云 OSS 上传工具 - 支持 V4 签名", long_about = None, disable_version_flag = true)]
struct Cli {
    /// 打印版本信息
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    version: (),

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 配置阿里云 OSS 凭证
    Config {
        /// OSS Endpoint
        #[arg(long)]
        endpoint: String,

        /// Access Key ID
        #[arg(long)]
        key_id: String,

        /// Access Key Secret
        #[arg(long)]
        key_secret: String,

        /// Bucket 名称
        #[arg(long, default_value = "")]
        bucket: String,
    },

    /// 上传文件到 OSS
    Upload {
        /// 本地文件路径 (可指定多个)
        #[arg(short, long, required = true, num_args = 1..)]
        file: Vec<PathBuf>,

        /// OSS 目标目录路径 (例如: test-upload/test)
        #[arg(short, long, required = true)]
        remote: String,

        /// 重命名上传文件 (仅单文件时有效)
        #[arg(long)]
        name: Option<String>,
    },

    /// 上传 Docker 文件到 OSS (自动生成 docker/{timestamp}/ 路径)
    UploadDocker {
        /// 本地文件路径 (可指定多个)
        #[arg(short, long, required = true, num_args = 1..)]
        file: Vec<PathBuf>,
    },

    /// 列出 OSS 中的文件
    List {
        /// 路径前缀
        #[arg(short, long, default_value = "")]
        prefix: String,
    },

    /// 删除 OSS 中的文件
    Rm {
        /// OSS 文件路径 (不含 bucket)
        #[arg(short, long)]
        remote: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Config {
            endpoint,
            key_id,
            key_secret,
            bucket,
        } => {
            let bucket = if bucket.is_empty() {
                println!("请输入 Bucket 名称:");
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                input.trim().to_string()
            } else {
                bucket
            };

            // Validate inputs before saving
            if !oss::validate_endpoint(&endpoint) {
                return Err(anyhow::anyhow!("无效的 endpoint: {}", endpoint));
            }
            if !oss::validate_bucket_name(&bucket) {
                return Err(anyhow::anyhow!(
                    "无效的 bucket 名称: {} (3-63字符，仅限小写字母、数字、短横线)",
                    bucket
                ));
            }

            let config = config::Config {
                endpoint,
                bucket_name: bucket,
                access_key_id: key_id,
                access_key_secret: key_secret,
                region: None,
                cdn_domain: None,
                path_prefix: None,
            };

            config.save()?;
            println!("✅ 配置已保存到 ~/.config/nuwax-ossutil.toml");
        }

        Commands::Upload { file, remote, name } => {
            commands::upload_files(&file, &remote, name.as_deref()).await?;
        }

        Commands::UploadDocker { file } => {
            commands::upload_docker_files(&file).await?;
        }

        Commands::List { prefix } => {
            commands::list_files(&prefix).await?;
        }

        Commands::Rm { remote } => {
            commands::delete_file(&remote).await?;
        }
    }

    Ok(())
}
