use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;

const CONFIG_FILE: &str = ".config/nuwax-ossutil.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub endpoint: String,
    pub bucket_name: String,
    pub access_key_id: String,
    pub access_key_secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cdn_domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;

        if config_path.exists() {
            let content = fs::read_to_string(&config_path)?;
            let config: Config = toml::from_str(&content)?;
            Ok(config)
        } else {
            Err(anyhow::anyhow!("配置文件不存在，请先运行 config 命令"))
        }
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(self)?;
        fs::write(&config_path, content)?;
        Ok(())
    }

    fn config_path() -> Result<PathBuf> {
        let home = env::var("HOME").map(PathBuf::from).map_err(|_| {
            anyhow::anyhow!("无法获取 HOME 目录，请设置 HOME 环境变量")
        })?;
        Ok(home.join(CONFIG_FILE))
    }
}

pub fn load_config() -> Result<Config> {
    let file_config = Config::load().ok();

    let endpoint = env::var("OSS_ENDPOINT")
        .ok()
        .or_else(|| file_config.as_ref().map(|c| c.endpoint.clone()))
        .ok_or_else(|| anyhow::anyhow!("未设置 OSS_ENDPOINT，请先运行 config 命令"))?;

    let bucket_name = env::var("OSS_BUCKET_NAME")
        .ok()
        .or_else(|| file_config.as_ref().map(|c| c.bucket_name.clone()))
        .ok_or_else(|| anyhow::anyhow!("未设置 OSS_BUCKET_NAME，请先运行 config 命令"))?;

    let access_key_id = env::var("OSS_ACCESS_KEY_ID")
        .ok()
        .or_else(|| file_config.as_ref().map(|c| c.access_key_id.clone()))
        .ok_or_else(|| anyhow::anyhow!("未设置 OSS_ACCESS_KEY_ID，请先运行 config 命令"))?;

    let access_key_secret = env::var("OSS_ACCESS_KEY_SECRET")
        .ok()
        .or_else(|| file_config.as_ref().map(|c| c.access_key_secret.clone()))
        .ok_or_else(|| anyhow::anyhow!("未设置 OSS_ACCESS_KEY_SECRET，请先运行 config 命令"))?;

    let region = env::var("OSS_REGION")
        .ok()
        .or_else(|| file_config.as_ref().and_then(|c| c.region.clone()));

    let cdn_domain = env::var("OSS_CDN_DOMAIN")
        .ok()
        .or_else(|| file_config.as_ref().and_then(|c| c.cdn_domain.clone()));

    let path_prefix = env::var("OSS_PATH_PREFIX")
        .ok()
        .or_else(|| file_config.as_ref().and_then(|c| c.path_prefix.clone()));

    Ok(Config {
        endpoint,
        bucket_name,
        access_key_id,
        access_key_secret,
        region,
        cdn_domain,
        path_prefix,
    })
}
