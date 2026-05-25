use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use futures::future::join_all;
use hmac::{Hmac, KeyInit, Mac};
use quick_xml::events::Event;
use quick_xml::name::QName;
use quick_xml::Reader;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::Semaphore;

use crate::config::Config;
use super::error::{OssApiError, OssError};
use super::validate::get_region_from_endpoint;

type Result<T> = std::result::Result<T, OssError>;

/// Progress callback: called with cumulative bytes transferred after each part/simple upload completes.
pub type ProgressCallback = Arc<dyn Fn(u64) + Send + Sync + 'static>;

/// Query parameters: key → value. Empty string value means flag parameter (e.g. "uploads" → "").
type QueryParams = HashMap<String, String>;

const MULTIPART_THRESHOLD: u64 = 5 * 1024 * 1024; // 5MB
const PART_SIZE: u64 = 10 * 1024 * 1024; // 10MB per part
const MAX_CONCURRENT_UPLOADS: usize = 3;
const MAX_CONCURRENT_PARTS: usize = 3;
const MAX_PART_RETRIES: u32 = 3;
const CHECKPOINT_DIR: &str = ".cache/nuwax-ossutil";
const MAX_LIST_KEYS: &str = "1000";

/// Request body variants to avoid unnecessary allocations
enum RequestBody {
    Empty,
    Bytes(Vec<u8>),
    Text(String),
}

// ============================================================================
// UploadCheckpoint — 断点续传
// ============================================================================

/// Checkpoint for resumable multipart uploads (断点续传)
#[derive(Serialize, Deserialize)]
struct UploadCheckpoint {
    upload_id: String,
    remote_path: String,
    local_path: String,
    file_size: u64,
    total_parts: u32,
    /// 1-indexed: completed_parts[part_number - 1] = Some(etag) when done
    completed_parts: Vec<Option<String>>,
}

impl UploadCheckpoint {
    fn checkpoint_path(remote_path: &str, local_path: &str) -> std::path::PathBuf {
        use base64::Engine;
        let input = format!("{}|{}", remote_path, local_path);
        let hash = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha1::Sha1::digest(input.as_bytes()));
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        std::path::PathBuf::from(home)
            .join(CHECKPOINT_DIR)
            .join(format!("{}.checkpoint", hash))
    }

    fn save(&self) {
        let path = Self::checkpoint_path(&self.remote_path, &self.local_path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(self) {
            let _ = std::fs::write(&path, json);
        }
    }

    fn load(remote_path: &str, local_path: &str) -> Option<Self> {
        let path = Self::checkpoint_path(remote_path, local_path);
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn delete(&self) {
        let path = Self::checkpoint_path(&self.remote_path, &self.local_path);
        let _ = std::fs::remove_file(path);
    }

    fn can_resume(&self, remote_path: &str, local_path: &str, file_size: u64) -> bool {
        self.remote_path == remote_path
            && self.local_path == local_path
            && self.file_size == file_size
    }
}

// ============================================================================
// OssClient
// ============================================================================

#[derive(Clone)]
pub struct OssClient {
    access_key_id: String,
    access_key_secret: String,
    endpoint: String,
    bucket_name: String,
    region: String,
    scheme: String,
    cdn_domain: Option<String>,
    path_prefix: Option<String>,
    http_client: reqwest::Client,
    semaphore: Arc<Semaphore>,
}

impl OssClient {
    pub fn new(config: &Config) -> Result<Self> {
        // Fail Fast: region must be available
        let region = config
            .region
            .clone()
            .or_else(|| get_region_from_endpoint(&config.endpoint))
            .ok_or_else(|| {
                OssError::InvalidEndpoint(format!(
                    "无法从 endpoint '{}' 推断 region，请设置 region 配置或 OSS_REGION 环境变量",
                    config.endpoint
                ))
            })?;

        // Determine scheme from endpoint
        let scheme = if config.endpoint.starts_with("http://") {
            "http"
        } else {
            "https"
        }
        .to_string();

        // Strip scheme from endpoint if present
        let endpoint = config
            .endpoint
            .strip_prefix("https://")
            .or_else(|| config.endpoint.strip_prefix("http://"))
            .unwrap_or(&config.endpoint)
            .to_string();

        Ok(Self {
            access_key_id: config.access_key_id.clone(),
            access_key_secret: config.access_key_secret.clone(),
            endpoint,
            bucket_name: config.bucket_name.clone(),
            region,
            scheme,
            cdn_domain: config.cdn_domain.clone(),
            path_prefix: config.path_prefix.clone(),
            http_client: reqwest::Client::new(),
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_UPLOADS)),
        })
    }

    /// Build full remote path with optional path_prefix
    pub fn full_remote_path(&self, remote_path: &str) -> String {
        match &self.path_prefix {
            Some(prefix) if !prefix.is_empty() => {
                format!(
                    "{}/{}",
                    prefix.trim_end_matches('/'),
                    remote_path.trim_start_matches('/')
                )
            }
            _ => remote_path.to_string(),
        }
    }

    // ========================================================================
    // Core request infrastructure
    // ========================================================================

    /// Execute an HTTP request with all OSS boilerplate:
    /// - Unified timestamp (prevents Date/x-oss-date mismatch)
    /// - Standard V4 headers (x-oss-content-sha256, x-oss-date, date)
    /// - V4 signature + Authorization header
    /// - Error response parsing (XML → OssApiError, fallback to text)
    async fn do_request(
        &self,
        method: &str,
        object_key: &str,
        query: &QueryParams,
        extra_headers: &HeaderMap,
        body: RequestBody,
    ) -> Result<reqwest::Response> {
        let url = self.build_url_with_query(object_key, query);

        // Single timestamp for both headers — prevents mismatch at minute boundaries
        let now = Utc::now();
        let gmt_date = now.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let oss_date = now.format("%Y%m%dT%H%M%SZ").to_string();

        // Build headers: start with caller-provided, add standard headers
        let mut headers = extra_headers.clone();
        headers.insert(
            "date",
            gmt_date
                .parse()
                .map_err(|e| OssError::Other(format!("invalid date header: {}", e)))?,
        );
        headers.insert("x-oss-content-sha256", "UNSIGNED-PAYLOAD".parse().unwrap());
        headers.insert(
            "x-oss-date",
            oss_date
                .parse()
                .map_err(|e| OssError::Other(format!("invalid x-oss-date header: {}", e)))?,
        );

        // V4 sign
        let authorization = self.sign_request(method, object_key, query, &headers);

        // Build and send request
        let mut req = match method {
            "PUT" => self.http_client.put(&url),
            "POST" => self.http_client.post(&url),
            "DELETE" => self.http_client.delete(&url),
            "HEAD" => self.http_client.head(&url),
            _ => self.http_client.get(&url),
        };

        req = req.headers(headers).header("authorization", &authorization);

        req = match body {
            RequestBody::Empty => req,
            RequestBody::Bytes(b) => req.body(b),
            RequestBody::Text(s) => req.body(s),
        };

        let response = req.send().await?;

        if !response.status().is_success() {
            return Err(self.handle_error_response(method, &url, response).await);
        }

        Ok(response)
    }

    /// Parse error response body as OssApiError, fallback to raw text.
    async fn handle_error_response(
        &self,
        method: &str,
        url: &str,
        response: reqwest::Response,
    ) -> OssError {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if body.trim_start().starts_with("<?xml") || body.trim_start().starts_with("<Error") {
            let api_error = OssApiError::from_xml(&body);
            if !api_error.code.is_empty() {
                return OssError::Api(Box::new(api_error));
            }
        }

        // Show full diagnostics when XML parsing fails to extract error details
        if body.is_empty() {
            OssError::Other(format!("{} {} → HTTP {} (no response body)", method, url, status))
        } else {
            OssError::Other(format!(
                "{} {} → HTTP {}: {}",
                method,
                url,
                status,
                body.trim()
            ))
        }
    }

    // ========================================================================
    // Public API
    // ========================================================================

    /// Smart upload: simple PUT for < 5MB, multipart for >= 5MB.
    /// Includes concurrency control via semaphore.
    /// `on_progress` is called with cumulative bytes transferred after each part/simple upload completes.
    pub async fn upload_file_smart(
        &self,
        local_path: &str,
        remote_path: &str,
        content_type: &str,
        on_progress: Option<ProgressCallback>,
    ) -> Result<String> {
        let _permit = self.semaphore.acquire().await.map_err(|e| {
            OssError::Other(format!("获取上传信号量失败: {}", e))
        })?;

        let full_path = self.full_remote_path(remote_path);
        let file_size = tokio::fs::metadata(local_path)
            .await
            .map_err(|e| OssError::Other(format!("读取文件元数据失败: {}: {}", local_path, e)))?
            .len();

        if file_size >= MULTIPART_THRESHOLD {
            self.multipart_upload(local_path, &full_path, content_type, file_size, on_progress)
                .await
        } else {
            self.simple_upload(local_path, &full_path, content_type, file_size, on_progress)
                .await
        }
    }

    /// List all objects with given prefix, automatically handling pagination.
    pub async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let mut all_keys = Vec::new();
        let mut marker = String::new();

        loop {
            let mut query = QueryParams::new();
            if !prefix.is_empty() {
                query.insert("prefix".to_string(), prefix.to_string());
            }
            if !marker.is_empty() {
                query.insert("marker".to_string(), marker.clone());
            }
            query.insert("max-keys".to_string(), MAX_LIST_KEYS.to_string());

            let response = self
                .do_request("GET", "", &query, &HeaderMap::new(), RequestBody::Empty)
                .await?;

            let body = response.text().await?;
            let (keys, is_truncated, next_marker) = Self::parse_list_response(&body);

            if keys.is_empty() && is_truncated {
                break;
            }

            if let Some(last) = keys.last() {
                marker = next_marker.unwrap_or_else(|| last.clone());
            }
            all_keys.extend(keys);

            if !is_truncated {
                break;
            }
        }

        Ok(all_keys)
    }

    pub async fn delete(&self, remote_path: &str) -> Result<()> {
        self.do_request(
            "DELETE",
            remote_path,
            &QueryParams::new(),
            &HeaderMap::new(),
            RequestBody::Empty,
        )
        .await?;
        Ok(())
    }

    // ========================================================================
    // Upload internals
    // ========================================================================

    /// Simple PUT for small files (< 5MB)
    async fn simple_upload(
        &self,
        local_path: &str,
        remote_path: &str,
        content_type: &str,
        file_size: u64,
        on_progress: Option<ProgressCallback>,
    ) -> Result<String> {
        let file_data = tokio::fs::read(local_path).await?;

        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            content_type
                .parse()
                .map_err(|e| OssError::Other(format!("invalid content-type: {}", e)))?,
        );
        headers.insert("content-length", file_data.len().to_string().parse().unwrap());

        self.do_request(
            "PUT",
            remote_path,
            &QueryParams::new(),
            &headers,
            RequestBody::Bytes(file_data),
        )
        .await?;

        if let Some(cb) = &on_progress {
            cb(file_size);
        }

        Ok(self.generate_download_url(remote_path))
    }

    /// Multipart upload for large files (>= 5MB)
    ///
    /// - Real streaming: semaphore acquired BEFORE reading each part (bounded memory)
    /// - Retry: each part retried up to 3 times with exponential backoff
    /// - Checkpoint: interrupted uploads resume from last completed part
    /// - Progress: `on_progress` is called with cumulative bytes after each part completes
    async fn multipart_upload(
        &self,
        local_path: &str,
        remote_path: &str,
        content_type: &str,
        file_size: u64,
        on_progress: Option<ProgressCallback>,
    ) -> Result<String> {
        let total_parts = file_size.div_ceil(PART_SIZE) as u32;

        // Checkpoint: try to resume an interrupted upload
        let (upload_id, mut completed_parts, resume_bytes) =
            match UploadCheckpoint::load(remote_path, local_path) {
                Some(cp) if cp.can_resume(remote_path, local_path, file_size) => {
                    let done = cp.completed_parts.iter().filter(|p| p.is_some()).count();
                    let bytes: u64 = cp.completed_parts.iter().enumerate().filter_map(|(i, p)| {
                        p.as_ref().map(|_| {
                            let offset = i as u64 * PART_SIZE;
                            std::cmp::min(PART_SIZE, file_size - offset)
                        })
                    }).sum();
                    println!(
                        "   断点续传: 已完成 {}/{} 分片, UploadId: {}",
                        done, cp.total_parts, cp.upload_id
                    );
                    (cp.upload_id, cp.completed_parts, bytes)
                }
                Some(stale_cp) => {
                    let _ = self.abort_multipart(remote_path, &stale_cp.upload_id).await;
                    let upload_id = self.initiate_multipart(remote_path, content_type).await?;
                    (upload_id, vec![None; total_parts as usize], 0u64)
                }
                None => {
                    let upload_id = self.initiate_multipart(remote_path, content_type).await?;
                    (upload_id, vec![None; total_parts as usize], 0u64)
                }
            };

        // Notify progress for already-completed parts (resume)
        if resume_bytes > 0
            && let Some(cb) = &on_progress {
            cb(resume_bytes);
        }

        let mut file = tokio::fs::File::open(local_path).await?;

        let part_semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_PARTS));
        let mut part_futures = Vec::new();

        for part_number in 1..=total_parts {
            // Skip already-completed parts (resume from checkpoint)
            if completed_parts[(part_number - 1) as usize].is_some() {
                continue;
            }

            let offset = (part_number - 1) as u64 * PART_SIZE;
            let part_size = std::cmp::min(PART_SIZE, file_size - offset) as usize;

            // CRITICAL FIX: acquire semaphore BEFORE reading to bound memory.
            let permit = Arc::clone(&part_semaphore)
                .acquire_owned()
                .await
                .map_err(|e| OssError::Other(format!("获取分片信号量失败: {}", e)))?;

            let mut buffer = vec![0u8; part_size];
            file.seek(SeekFrom::Start(offset)).await?;
            file.read_exact(&mut buffer).await?;

            let client = self.clone();
            let uid = upload_id.clone();
            let rp = remote_path.to_string();
            let cb = on_progress.clone();
            let part_bytes = part_size as u64;

            part_futures.push(async move {
                let _permit = permit;

                let mut last_err = None;
                for attempt in 1..=MAX_PART_RETRIES {
                    let is_last = attempt == MAX_PART_RETRIES;
                    let data = buffer.clone();

                    match client.upload_part(uid.clone(), part_number, data, &rp).await {
                        Ok(etag) => {
                            if let Some(cb) = &cb {
                                cb(part_bytes);
                            }
                            return Ok((part_number, etag));
                        }
                        Err(e) => {
                            if !is_last {
                                let delay =
                                    std::time::Duration::from_secs(1 << (attempt - 1));
                                tokio::time::sleep(delay).await;
                            }
                            last_err = Some(e);
                        }
                    }
                }
                Err(last_err.unwrap())
            });
        }

        let part_results = join_all(part_futures).await;

        let mut has_error = false;
        for result in part_results {
            match result {
                Ok((part_number, etag)) => {
                    completed_parts[(part_number - 1) as usize] = Some(etag);
                    UploadCheckpoint {
                        upload_id: upload_id.clone(),
                        remote_path: remote_path.to_string(),
                        local_path: local_path.to_string(),
                        file_size,
                        total_parts,
                        completed_parts: completed_parts.clone(),
                    }
                    .save();
                }
                Err(e) => {
                    has_error = true;
                    eprintln!("   分片上传失败: {}", e);
                }
            }
        }

        if has_error {
            let done = completed_parts.iter().filter(|p| p.is_some()).count();
            return Err(OssError::Other(format!(
                "部分分片上传失败 ({}/{} 已完成), 再次执行命令可从断点恢复",
                done, total_parts
            )));
        }

        // All parts succeeded — clean up checkpoint and complete
        UploadCheckpoint {
            upload_id: upload_id.clone(),
            remote_path: remote_path.to_string(),
            local_path: local_path.to_string(),
            file_size,
            total_parts,
            completed_parts: completed_parts.clone(),
        }
        .delete();

        let etags: Vec<String> = completed_parts.into_iter().flatten().collect();
        self.complete_multipart(remote_path, &upload_id, etags).await
    }

    /// Initiate multipart upload, returns UploadId
    async fn initiate_multipart(&self, remote_path: &str, content_type: &str) -> Result<String> {
        let mut query = QueryParams::new();
        query.insert("uploads".to_string(), String::new());

        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            content_type
                .parse()
                .map_err(|e| OssError::Other(format!("invalid content-type: {}", e)))?,
        );

        let response = self
            .do_request("POST", remote_path, &query, &headers, RequestBody::Empty)
            .await?;
        let body = response.text().await?;

        Self::parse_upload_id(&body)
    }

    /// Upload a single part
    async fn upload_part(
        &self,
        upload_id: String,
        part_number: u32,
        data: Vec<u8>,
        remote_path: &str,
    ) -> Result<String> {
        let mut query = QueryParams::new();
        query.insert("partNumber".to_string(), part_number.to_string());
        query.insert("uploadId".to_string(), upload_id);

        let mut headers = HeaderMap::new();
        headers.insert("content-length", data.len().to_string().parse().unwrap());

        let response = self
            .do_request(
                "PUT",
                remote_path,
                &query,
                &headers,
                RequestBody::Bytes(data),
            )
            .await?;

        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();

        if etag.is_empty() {
            return Err(OssError::Other("分片上传响应中未找到 ETag".to_string()));
        }

        Ok(etag)
    }

    /// Complete multipart upload by sending XML with all part ETags
    async fn complete_multipart(
        &self,
        remote_path: &str,
        upload_id: &str,
        etags: Vec<String>,
    ) -> Result<String> {
        let mut query = QueryParams::new();
        query.insert("uploadId".to_string(), upload_id.to_string());

        // Build XML body — escape ETag values for XML safety
        let parts_xml: String = etags
            .iter()
            .enumerate()
            .map(|(i, etag)| {
                let safe_etag = etag
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('"', "&quot;");
                format!(
                    "<Part><PartNumber>{}</PartNumber><ETag>\"{}\"</ETag></Part>",
                    i + 1,
                    safe_etag
                )
            })
            .collect();
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><CompleteMultipartUpload>{}</CompleteMultipartUpload>",
            parts_xml
        );

        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/xml".parse().unwrap());
        headers.insert("content-length", body.len().to_string().parse().unwrap());

        self.do_request(
            "POST",
            remote_path,
            &query,
            &headers,
            RequestBody::Text(body),
        )
        .await?;

        Ok(self.generate_download_url(remote_path))
    }

    /// Abort a multipart upload to clean up orphaned parts on OSS.
    pub async fn abort_multipart(&self, remote_path: &str, upload_id: &str) -> Result<()> {
        let mut query = QueryParams::new();
        query.insert("uploadId".to_string(), upload_id.to_string());

        self.do_request("DELETE", remote_path, &query, &HeaderMap::new(), RequestBody::Empty)
            .await?;

        Ok(())
    }

    // ========================================================================
    // Helper methods
    // ========================================================================

    fn build_url(&self, remote_path: &str) -> String {
        let path = remote_path.trim_start_matches('/');
        if path.is_empty() {
            format!("{}://{}.{}/", self.scheme, self.bucket_name, self.endpoint)
        } else {
            format!(
                "{}://{}.{}/{}",
                self.scheme, self.bucket_name, self.endpoint, path
            )
        }
    }

    fn build_url_with_query(&self, remote_path: &str, query: &QueryParams) -> String {
        let base = self.build_url(remote_path);
        let qs = Self::build_canonical_query_string(query);
        if qs.is_empty() {
            base
        } else {
            format!("{}?{}", base, qs)
        }
    }

    fn generate_download_url(&self, remote_path: &str) -> String {
        match &self.cdn_domain {
            Some(domain) if !domain.is_empty() => format!(
                "{}/{}",
                domain.trim_end_matches('/'),
                remote_path.trim_start_matches('/')
            ),
            _ => self.build_url(remote_path),
        }
    }

    // ========================================================================
    // V4 signing
    // ========================================================================

    /// Build canonical query string: URL-encoded keys/values, sorted by key.
    fn build_canonical_query_string(query: &QueryParams) -> String {
        if query.is_empty() {
            return String::new();
        }

        let mut pairs: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| {
                (
                    urlencoding::encode(k).to_string(),
                    urlencoding::encode(v).to_string(),
                )
            })
            .collect();

        pairs.sort_by(|a, b| a.0.cmp(&b.0));

        pairs
            .iter()
            .map(|(k, v)| {
                if v.is_empty() {
                    k.clone()
                } else {
                    format!("{}={}", k, v)
                }
            })
            .collect::<Vec<_>>()
            .join("&")
    }

    /// Build canonical URI: path segments individually URL-encoded.
    fn build_canonical_uri(&self, object_key: &str) -> String {
        if object_key.is_empty() {
            return format!("/{}/", urlencoding::encode(&self.bucket_name));
        }

        let encoded_key = object_key
            .split('/')
            .map(|s| urlencoding::encode(s).to_string())
            .collect::<Vec<_>>()
            .join("/");

        format!(
            "/{}/{}",
            urlencoding::encode(&self.bucket_name),
            encoded_key
        )
    }

    /// Build canonical headers: only x-oss-content-sha256 (required), Content-Type,
    /// Content-MD5, and x-oss-* headers participate in V4 signing.
    /// The `host` header is NOT included unless listed in AdditionalHeaders.
    /// Each header line ends with a trailing \n per the V4 spec.
    fn build_canonical_headers(&self, headers: &HeaderMap) -> String {
        let mut canonical: Vec<(String, String)> = Vec::new();

        for (name, value) in headers.iter() {
            let key = name.as_str().to_lowercase();
            if key == "content-type" || key == "content-md5" || key.starts_with("x-oss-") {
                canonical.push((key, value.to_str().unwrap_or("").trim().to_string()));
            }
        }

        canonical.sort_by(|a, b| a.0.cmp(&b.0));

        canonical
            .iter()
            .map(|(k, v)| format!("{}:{}\n", k, v))
            .collect::<Vec<_>>()
            .join("")
    }

    /// Derive V4 signing key:
    ///   "aliyun_v4" + secret → HMAC(date) → HMAC(region) → HMAC("oss") → HMAC("aliyun_v4_request")
    fn derive_signing_key(&self, date_string: &str) -> Vec<u8> {
        let key_string = format!("aliyun_v4{}", self.access_key_secret);
        let date_key = Self::hmac_sha256(key_string.as_bytes(), date_string.as_bytes());
        let date_region_key = Self::hmac_sha256(&date_key, self.region.as_bytes());
        let date_region_service_key = Self::hmac_sha256(&date_region_key, b"oss");
        Self::hmac_sha256(&date_region_service_key, b"aliyun_v4_request")
    }

    /// V4 signing: produce the Authorization header value.
    ///
    /// Canonical request format (6 components per OSS V4 spec):
    /// ```text
    /// HTTPVerb\n
    /// CanonicalURI\n
    /// CanonicalQueryString\n
    /// CanonicalHeaders\n
    /// AdditionalHeaders\n
    /// HashedPayLoad
    /// ```
    fn sign_request(
        &self,
        method: &str,
        object_key: &str,
        query: &QueryParams,
        headers: &HeaderMap,
    ) -> String {
        let date_time_string = headers
            .get("x-oss-date")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let date_string = &date_time_string[..8];

        // Build canonical request (6 components)
        let canonical_uri = self.build_canonical_uri(object_key);
        let canonical_query = Self::build_canonical_query_string(query);
        let canonical_headers = self.build_canonical_headers(headers);
        // AdditionalHeaders: empty (no optional headers like host are signed)
        let additional_headers = "";
        // canonical_headers ends with a trailing \n (each header line has \n).
        // The format inserts \n between canonical_headers and additional_headers,
        // and another \n between additional_headers and UNSIGNED-PAYLOAD.
        // Result: ...last-header\n + \n + (empty) + \n + UNSIGNED-PAYLOAD = 3 \n total
        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\nUNSIGNED-PAYLOAD",
            method, canonical_uri, canonical_query, canonical_headers, additional_headers
        );

        log::debug!("canonical request:\n{}", canonical_request);

        // Build string to sign
        let canonical_request_hash = Self::sha256_hex(canonical_request.as_bytes());
        let string_to_sign = format!(
            "OSS4-HMAC-SHA256\n{}\n{}/{}/oss/aliyun_v4_request\n{}",
            date_time_string, date_string, self.region, canonical_request_hash
        );

        log::debug!("string to sign:\n{}", string_to_sign);

        // Derive signing key and compute signature
        let signing_key = self.derive_signing_key(date_string);
        let signature = hex::encode(Self::hmac_sha256(&signing_key, string_to_sign.as_bytes()));

        log::debug!("signature: {}", signature);

        // Build Authorization header (no AdditionalHeaders field since it's empty)
        format!(
            "OSS4-HMAC-SHA256 Credential={}/{}/{}/oss/aliyun_v4_request,Signature={}",
            self.access_key_id, date_string, self.region, signature
        )
    }

    fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    fn sha256_hex(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hex::encode(hasher.finalize())
    }

    // ========================================================================
    // XML parsing
    // ========================================================================

    fn parse_upload_id(xml: &str) -> Result<String> {
        let mut reader = Reader::from_str(xml);
        let mut in_upload_id = false;
        let mut text_buf = String::new();
        let mut upload_id = String::new();

        loop {
            match reader.read_event() {
                Ok(Event::Start(ref e)) if e.name() == QName(b"UploadId") => {
                    in_upload_id = true;
                    text_buf.clear();
                }
                Ok(Event::Text(ref e)) if in_upload_id => {
                    text_buf.push_str(&String::from_utf8_lossy(e));
                }
                Ok(Event::End(ref e)) if e.name() == QName(b"UploadId") => {
                    upload_id = text_buf.trim().to_string();
                    in_upload_id = false;
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(OssError::XmlParse(format!("解析 XML 失败: {}", e))),
                _ => {}
            }
        }

        if upload_id.is_empty() {
            return Err(OssError::XmlParse("响应中未找到 UploadId".to_string()));
        }

        Ok(upload_id)
    }

    /// Parse ListBucketResult XML, returning (keys, is_truncated, next_marker).
    fn parse_list_response(xml: &str) -> (Vec<String>, bool, Option<String>) {
        let mut keys = Vec::new();
        let mut reader = Reader::from_str(xml);
        let mut current_tag = String::new();
        let mut text_buf = String::new();
        let mut is_truncated = false;
        let mut next_marker: Option<String> = None;

        loop {
            match reader.read_event() {
                Ok(Event::Start(ref e)) => {
                    let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                    if matches!(name.as_str(), "Key" | "IsTruncated" | "NextMarker") {
                        current_tag = name;
                        text_buf.clear();
                    }
                }
                Ok(Event::Text(ref e)) if !current_tag.is_empty() => {
                    text_buf.push_str(&String::from_utf8_lossy(e));
                }
                Ok(Event::End(ref e)) => {
                    let tag = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                    if !current_tag.is_empty() && tag == current_tag {
                        let trimmed = text_buf.trim().to_string();
                        match current_tag.as_str() {
                            "Key" => keys.push(trimmed),
                            "IsTruncated" => is_truncated = trimmed == "true",
                            "NextMarker" => next_marker = Some(trimmed),
                            _ => {}
                        }
                        current_tag.clear();
                        text_buf.clear();
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
        }

        (keys, is_truncated, next_marker)
    }
}
