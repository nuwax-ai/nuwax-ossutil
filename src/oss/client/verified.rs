//! Strict release uploads. Legacy upload behaviour is intentionally unchanged.
use super::*;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fingerprint {
    pub size: u64,
    pub sha256: String,
    pub crc64: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ObjectStat {
    pub bucket: String,
    pub endpoint: String,
    pub key: String,
    pub size: u64,
    pub crc64: String,
    pub request_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VerifiedReceipt {
    pub protocol: String,
    pub local: String,
    pub url: String,
    pub fingerprint: Fingerprint,
    pub object: ObjectStat,
    pub forbid_overwrite: bool,
    pub verified: bool,
}

#[derive(Serialize, Deserialize)]
struct StrictCheckpoint {
    protocol: String,
    bucket: String,
    endpoint: String,
    key: String,
    local: String,
    fingerprint: Fingerprint,
    upload_id: String,
    etags: Vec<String>,
}

async fn load_checkpoint(path: &Path) -> Result<Option<StrictCheckpoint>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            OssError::Other(format!(
                "corrupt strict checkpoint; preserve the original upload identity: {error}"
            ))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

// OSS uses reflected ECMA-182, initial/final XOR all ones (CRC-64/XZ).
fn crc_update(crc: u64, bytes: &[u8]) -> u64 {
    static TABLE: OnceLock<[u64; 256]> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0u64; 256];
        for (i, value) in table.iter_mut().enumerate() {
            let mut c = i as u64;
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    (c >> 1) ^ 0xc96c5795d7870f42
                } else {
                    c >> 1
                };
            }
            *value = c;
        }
        table
    });
    let mut current = !crc;
    for b in bytes {
        current = table[((current as u8) ^ b) as usize] ^ (current >> 8);
    }
    !current
}

pub async fn fingerprint(path: &Path) -> Result<Fingerprint> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut sha = Sha256::new();
    let mut crc = 0;
    let mut size = 0;
    let mut buffer = vec![0; 4 * 1024 * 1024];
    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        sha.update(&buffer[..n]);
        crc = crc_update(crc, &buffer[..n]);
        size += n as u64;
    }
    Ok(Fingerprint {
        size,
        sha256: hex::encode(sha.finalize()),
        crc64: crc.to_string(),
    })
}

fn xml_value(xml: &str, tag: &[u8]) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == tag => {
                return reader
                    .read_text(e.name())
                    .map(|s| String::from_utf8_lossy(s.as_ref()).into_owned())
                    .map_err(|e| OssError::Other(format!("invalid OSS XML: {e}")));
            }
            Ok(Event::Eof) => return Err(OssError::Other("missing OSS XML field".into())),
            Err(e) => return Err(OssError::Other(format!("invalid OSS XML: {e}"))),
            _ => {}
        }
    }
}

fn check_versioning(xml: &str) -> Result<()> {
    let mut reader = Reader::from_str(xml);
    let mut root = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"VersioningConfiguration" => {
                root = true
            }
            Ok(Event::Empty(e)) if e.local_name().as_ref() == b"VersioningConfiguration" => {
                root = true
            }
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"Status" => {
                let status = reader
                    .read_text(e.name())
                    .map_err(|e| OssError::Other(e.to_string()))?;
                let status = String::from_utf8_lossy(status.as_ref()).into_owned();
                if !status.trim().is_empty() {
                    return Err(OssError::Other(format!(
                        "strict upload requires never-enabled bucket versioning, got {status}"
                    )));
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(OssError::Other(e.to_string())),
            _ => {}
        }
    }
    if !root {
        return Err(OssError::Other(
            "cannot establish bucket versioning state".into(),
        ));
    }
    Ok(())
}

fn assert_object(expected: &Fingerprint, object: &ObjectStat) -> Result<()> {
    if expected.size != object.size || expected.crc64 != object.crc64 {
        return Err(OssError::Other(format!(
            "OSS CRC64/size mismatch for {}: local {}/{}, remote {}/{}",
            object.key, expected.crc64, expected.size, object.crc64, object.size
        )));
    }
    Ok(())
}

fn checkpoint_lock(path: &Path) -> Result<std::fs::File> {
    #[cfg(unix)]
    {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))?;
        use std::os::fd::AsRawFd;
        unsafe extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
            return Err(OssError::Other(
                "another strict upload owns this object checkpoint".into(),
            ));
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(OssError::Other(
            "strict release uploads currently require macOS or Linux".into(),
        ))
    }
}

impl OssClient {
    pub fn new_verified(config: &Config) -> Result<Self> {
        let mut client = Self::new(config)?;
        client.http_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(300))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(client)
    }
    pub async fn check_release_bucket(
        &self,
        expected_bucket: &str,
        expected_endpoint: &str,
    ) -> Result<()> {
        if self.bucket_name != expected_bucket
            || self.endpoint != expected_endpoint
            || self.scheme != "https"
            || self.path_prefix.as_deref().is_some_and(|v| !v.is_empty())
        {
            return Err(OssError::Other(
                "strict upload bucket/endpoint/prefix configuration mismatch".into(),
            ));
        }
        let query = QueryParams::from([("versioning".to_string(), String::new())]);
        let response = self
            .do_request("GET", "", &query, &HeaderMap::new(), RequestBody::Empty)
            .await?;
        check_versioning(&response.text().await?)
    }

    pub async fn stat_verified(&self, key: &str) -> Result<ObjectStat> {
        let response = self
            .do_request(
                "HEAD",
                key,
                &QueryParams::new(),
                &HeaderMap::new(),
                RequestBody::Empty,
            )
            .await?;
        let header = |name: &str| -> Result<String> {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
                .ok_or_else(|| OssError::Other(format!("OSS HEAD missing {name}")))
        };
        let crc64 = header("x-oss-hash-crc64ecma")?;
        crc64
            .parse::<u64>()
            .map_err(|_| OssError::Other("invalid OSS CRC64".into()))?;
        Ok(ObjectStat {
            bucket: self.bucket_name.clone(),
            endpoint: self.endpoint.clone(),
            key: key.into(),
            size: header("content-length")?
                .parse()
                .map_err(|_| OssError::Other("invalid OSS size".into()))?,
            crc64,
            request_id: header("x-oss-request-id")?,
        })
    }

    /// Serial bounded-memory multipart uploader with fingerprint-bound checkpoints.
    pub async fn upload_verified(&self, path: &Path, key: &str) -> Result<VerifiedReceipt> {
        let local = path.canonicalize()?.to_string_lossy().into_owned();
        let initial = fingerprint(path).await?;
        let checkpoint_key = format!(
            "strict|{}|{}|{}|{}",
            self.bucket_name, self.endpoint, key, local
        );
        let cp_name = hex::encode(Sha256::digest(checkpoint_key.as_bytes()));
        let cp_path = path
            .parent()
            .unwrap_or(Path::new("."))
            .join(format!(".nuwax-strict-{cp_name}.checkpoint"));
        let _lock = checkpoint_lock(&cp_path)?;
        let protected_headers = || {
            let mut headers = HeaderMap::new();
            headers.insert("x-oss-forbid-overwrite", "true".parse().unwrap());
            headers
        };
        if initial.size < MULTIPART_THRESHOLD {
            let bytes = tokio::fs::read(path).await?;
            if hex::encode(Sha256::digest(&bytes)) != initial.sha256 {
                return Err(OssError::Other("local file changed before upload".into()));
            }
            let mut headers = protected_headers();
            headers.insert(
                "content-type",
                super::super::guess_content_type(key).parse()?,
            );
            self.do_request(
                "PUT",
                key,
                &QueryParams::new(),
                &headers,
                RequestBody::Bytes(bytes),
            )
            .await?;
        } else {
            let existing = load_checkpoint(&cp_path).await?;
            let total_parts = initial.size.div_ceil(PART_SIZE) as usize;
            let mut cp = match existing {
                Some(cp)
                    if cp.protocol == "nuwax-strict-upload-v1"
                        && cp.bucket == self.bucket_name
                        && cp.endpoint == self.endpoint
                        && cp.key == key
                        && cp.local == local
                        && cp.fingerprint == initial
                        && !cp.upload_id.is_empty()
                        && cp.etags.iter().all(|value| !value.is_empty())
                        && cp.etags.len() <= total_parts =>
                {
                    cp
                }
                Some(_) => {
                    return Err(OssError::Other(
                        "strict checkpoint identity changed; use a new object key".into(),
                    ));
                }
                None => {
                    let query = QueryParams::from([("uploads".to_string(), String::new())]);
                    let response = self
                        .do_request(
                            "POST",
                            key,
                            &query,
                            &protected_headers(),
                            RequestBody::Empty,
                        )
                        .await?;
                    StrictCheckpoint {
                        protocol: "nuwax-strict-upload-v1".into(),
                        bucket: self.bucket_name.clone(),
                        endpoint: self.endpoint.clone(),
                        key: key.into(),
                        local: local.clone(),
                        fingerprint: initial.clone(),
                        upload_id: xml_value(&response.text().await?, b"UploadId")?,
                        etags: vec![],
                    }
                }
            };
            let save = |cp: &StrictCheckpoint| -> Result<()> {
                std::fs::create_dir_all(cp_path.parent().unwrap())?;
                let tmp = cp_path.with_extension("strict.tmp");
                std::fs::write(
                    &tmp,
                    serde_json::to_vec(cp).map_err(|e| OssError::Other(e.to_string()))?,
                )?;
                std::fs::rename(tmp, &cp_path)?;
                Ok(())
            };
            save(&cp)?;
            let mut file = tokio::fs::File::open(path).await?;
            file.seek(SeekFrom::Start(cp.etags.len() as u64 * PART_SIZE))
                .await?;
            for part in cp.etags.len()..total_parts {
                let n = (initial.size - part as u64 * PART_SIZE).min(PART_SIZE) as usize;
                let mut buffer = vec![0; n];
                file.read_exact(&mut buffer).await?;
                let mut last = None;
                for attempt in 0..MAX_PART_RETRIES {
                    match self
                        .upload_part(cp.upload_id.clone(), (part + 1) as u32, buffer.clone(), key)
                        .await
                    {
                        Ok(etag) => {
                            cp.etags.push(etag);
                            last = None;
                            break;
                        }
                        Err(error) => {
                            last = Some(error);
                            if attempt + 1 < MAX_PART_RETRIES {
                                tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
                            }
                        }
                    }
                }
                if let Some(error) = last {
                    return Err(error);
                }
                save(&cp)?;
                eprintln!(
                    "verified upload {}: {}/{} parts",
                    path.file_name().unwrap().to_string_lossy(),
                    part + 1,
                    total_parts
                );
            }
            if fingerprint(path).await? != initial {
                return Err(OssError::Other(
                    "local file changed during upload; refusing completion".into(),
                ));
            }
            let parts: String = cp
                .etags
                .iter()
                .enumerate()
                .map(|(i, etag)| {
                    let safe = etag
                        .replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('>', "&gt;")
                        .replace('"', "&quot;");
                    format!(
                        "<Part><PartNumber>{}</PartNumber><ETag>\"{}\"</ETag></Part>",
                        i + 1,
                        safe
                    )
                })
                .collect();
            let body = format!("<CompleteMultipartUpload>{parts}</CompleteMultipartUpload>");
            let query = QueryParams::from([("uploadId".to_string(), cp.upload_id.clone())]);
            let mut headers = protected_headers();
            headers.insert("content-type", "application/xml".parse()?);
            let response = self
                .do_request("POST", key, &query, &headers, RequestBody::Text(body))
                .await?;
            // OSS may return an embedded Error document even with HTTP 200.
            let response_body = response.text().await?;
            if xml_value(&response_body, b"Code").is_ok() {
                return Err(OssError::Other(
                    "OSS multipart completion returned an error document".into(),
                ));
            }
        }
        let object = self.stat_verified(key).await?;
        assert_object(&initial, &object)?;
        if fingerprint(path).await? != initial {
            return Err(OssError::Other("local file changed after upload".into()));
        }
        if cp_path.exists() {
            tokio::fs::remove_file(cp_path).await?;
        }
        Ok(VerifiedReceipt {
            protocol: "nuwax-verified-upload-v1".into(),
            local,
            url: self.build_url(key),
            fingerprint: initial,
            object,
            forbid_overwrite: true,
            verified: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[tokio::test]
    async fn corrupt_checkpoint_is_not_reinterpreted_as_a_new_upload() {
        let fixture = MockFile::new(1);
        let path = fixture.directory.join("corrupt.checkpoint");
        assert!(load_checkpoint(&path).await.unwrap().is_none());
        std::fs::write(&path, b"{broken").unwrap();
        let error = load_checkpoint(&path).await.err().unwrap().to_string();
        assert!(error.contains("preserve the original upload identity"));
        assert_eq!(std::fs::read(path).unwrap(), b"{broken");
    }

    struct MockFile {
        directory: std::path::PathBuf,
        path: std::path::PathBuf,
    }
    impl MockFile {
        fn new(size: usize) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let name = format!(
                "oss-strict-{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let directory = std::env::temp_dir().join(name);
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join("candidate.bin");
            std::fs::write(&path, vec![b'x'; size]).unwrap();
            Self { directory, path }
        }
        fn checkpoints(&self) -> usize {
            std::fs::read_dir(&self.directory)
                .unwrap()
                .filter(|p| {
                    p.as_ref()
                        .unwrap()
                        .path()
                        .extension()
                        .is_some_and(|v| v == "checkpoint")
                })
                .count()
        }
    }
    impl Drop for MockFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    // A real local HTTP transport: inspect signed requests, not mocked method returns.
    fn server(
        steps: Vec<(String, bool, u16, String, String)>,
    ) -> (OssClient, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let endpoint = format!("oss.example.test:{}", address.port());
        let client = OssClient {
            access_key_id: "fixture".into(),
            access_key_secret: "fixture".into(),
            endpoint,
            bucket_name: "bucket".into(),
            region: "fixture".into(),
            scheme: "http".into(),
            cdn_domain: None,
            path_prefix: None,
            http_client: reqwest::Client::builder()
                .no_proxy()
                .resolve("bucket.oss.example.test", address)
                .build()
                .unwrap(),
            semaphore: Arc::new(Semaphore::new(1)),
        };
        let thread = std::thread::spawn(move || {
            for (method, forbid, status, headers, body) in steps {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut bytes = vec![];
                let end = loop {
                    let mut buffer = [0; 8192];
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                assert!(head.starts_with(&method.to_lowercase()), "{head}");
                assert!(head.contains("authorization: oss4-hmac-sha256"));
                assert_eq!(head.contains("x-oss-forbid-overwrite: true"), forbid);
                let length = head
                    .lines()
                    .find_map(|s| {
                        s.strip_prefix("content-length: ")
                            .and_then(|v| v.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while bytes.len() - end < length {
                    let mut buffer = [0; 16384];
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                }
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nconnection: close\r\ncontent-length: {}\r\n{headers}\r\n{body}",
                    if method == "HEAD" {
                        headers
                            .lines()
                            .find_map(|s| {
                                s.to_lowercase()
                                    .strip_prefix("x-fixture-size: ")
                                    .and_then(|v| v.parse::<usize>().ok())
                            })
                            .unwrap_or(0)
                    } else {
                        body.len()
                    }
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (client, thread)
    }

    fn head(fp: &Fingerprint, crc: &str) -> (String, bool, u16, String, String) {
        (
            "HEAD".into(),
            false,
            200,
            format!(
                "x-fixture-size: {}\r\nx-oss-hash-crc64ecma: {crc}\r\nx-oss-request-id: fixture\r\n",
                fp.size
            ),
            String::new(),
        )
    }

    #[tokio::test]
    async fn simple_put_and_head_match() {
        let file = MockFile::new(100);
        let fp = fingerprint(&file.path).await.unwrap();
        let (client, server) = server(vec![
            ("PUT".into(), true, 200, String::new(), String::new()),
            head(&fp, &fp.crc64),
        ]);
        let receipt = client
            .upload_verified(&file.path, "beta/fixture.bin")
            .await
            .unwrap();
        assert_eq!(receipt.fingerprint, fp);
        assert!(receipt.verified);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn collision_and_crc_failure_are_errors() {
        let file = MockFile::new(100);
        let (client, thread) = server(vec![(
            "PUT".into(),
            true,
            409,
            String::new(),
            "<Error><Code>FileAlreadyExists</Code></Error>".into(),
        )]);
        assert!(
            client
                .upload_verified(&file.path, "beta/collision.bin")
                .await
                .is_err()
        );
        thread.join().unwrap();
        let fp = fingerprint(&file.path).await.unwrap();
        let (client, thread) = server(vec![
            ("PUT".into(), true, 200, String::new(), String::new()),
            head(&fp, "1"),
        ]);
        assert!(
            client
                .upload_verified(&file.path, "beta/badcrc.bin")
                .await
                .unwrap_err()
                .to_string()
                .contains("CRC64/size mismatch")
        );
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn multipart_retains_checkpoint_on_failed_completion_and_resumes() {
        let file = MockFile::new(MULTIPART_THRESHOLD as usize);
        let fp = fingerprint(&file.path).await.unwrap();
        let (client, thread) = server(vec![
            ("POST".into(), true, 200, String::new(), "<InitiateMultipartUploadResult><UploadId>original-operation</UploadId></InitiateMultipartUploadResult>".into()),
            ("PUT".into(), false, 200, "etag: fixture\r\n".into(), String::new()),
            ("POST".into(), true, 500, String::new(), "<Error><Code>InternalError</Code></Error>".into()),
            ("POST".into(), true, 200, String::new(), "<CompleteMultipartUploadResult/>".into()),
            head(&fp, &fp.crc64),
        ]);
        assert!(
            client
                .upload_verified(&file.path, "beta/multipart.bin")
                .await
                .is_err()
        );
        assert_eq!(file.checkpoints(), 1);
        let receipt = client
            .upload_verified(&file.path, "beta/multipart.bin")
            .await
            .unwrap();
        assert_eq!(receipt.fingerprint, fp);
        assert_eq!(file.checkpoints(), 0);
        thread.join().unwrap();
    }
    #[test]
    fn python_manifest_signature_vector() {
        let client = OssClient::new(&Config {
            endpoint: "oss-rg-china-mainland.aliyuncs.com".into(),
            bucket_name: "nuwa-packages".into(),
            region: Some("rg-china-mainland".into()),
            access_key_id: "fixture-id".into(),
            access_key_secret: "fixture-secret".into(),
            cdn_domain: None,
            path_prefix: None,
        })
        .unwrap();
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("x-oss-date", "20261023000000Z"),
            ("x-oss-content-sha256", "UNSIGNED-PAYLOAD"),
            ("content-type", "application/json"),
            ("x-oss-forbid-overwrite", "true"),
        ] {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        let value = client.sign_request(
            "PUT",
            "docker-version/history/beta/20261023000000.json",
            &QueryParams::new(),
            &headers,
        );
        assert!(value.ends_with(
            "Signature=6a23bebe47328f791462dc94ffa0fb9966ca2362e4eb6cef440949db0432dd8a"
        ));
    }

    #[test]
    fn crc_vectors_and_streaming() {
        assert_eq!(crc_update(0, b""), 0);
        assert_eq!(crc_update(0, b"123456789"), 0x995dc9bbdf1939fa);
        assert_eq!(
            crc_update(crc_update(0, b"1234"), b"56789"),
            crc_update(0, b"123456789")
        );
    }
    #[test]
    fn versioning_fails_closed() {
        assert!(check_versioning("<VersioningConfiguration/>").is_ok());
        for xml in [
            "<VersioningConfiguration><Status>Enabled</Status></VersioningConfiguration>",
            "<VersioningConfiguration><Status>Suspended</Status></VersioningConfiguration>",
            "<Error/>",
        ] {
            assert!(check_versioning(xml).is_err());
        }
    }
    #[test]
    fn crc_and_size_are_both_required() {
        let expected = Fingerprint {
            size: 10,
            sha256: "a".repeat(64),
            crc64: "7".into(),
        };
        let mut object = ObjectStat {
            bucket: "bucket".into(),
            endpoint: "endpoint".into(),
            key: "key".into(),
            size: 10,
            crc64: "7".into(),
            request_id: "request".into(),
        };
        assert!(assert_object(&expected, &object).is_ok());
        object.crc64 = "8".into();
        assert!(assert_object(&expected, &object).is_err());
        object.crc64 = "7".into();
        object.size = 9;
        assert!(assert_object(&expected, &object).is_err());
    }
}
