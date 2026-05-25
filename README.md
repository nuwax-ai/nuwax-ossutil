# nuwax-ossutil

阿里云 OSS 命令行上传工具，支持 OSS V1 签名认证。

对标服务端 `/api/v1/oss/upload/docker` 接口，提供完整的 Docker 文件上传能力：多文件并发上传、大文件自动分片、时间戳目录管理、CDN 域名下载链接生成。

---

## 功能特性

- **多文件并发上传**：最多 3 个文件并发上传，通过 Semaphore 控制并发数
- **大文件分片上传**：文件 ≥ 5MB 自动切换为分片上传（10MB/片，最多 3 片并发）
- **分片失败自动清理**：任一分片上传失败，自动调用 Abort Multipart Upload 清理已上传分片
- **时间戳目录**：`upload-docker` 命令自动生成 `docker/{YYYYMMDDHHMMSS}/` 路径，与服务端接口行为一致
- **CDN 域名支持**：可配置 CDN 域名，上传完成后返回 CDN 下载链接
- **路径前缀**：支持全局路径前缀，自动拼接在远程路径前
- **OSS V1 签名**：使用 HMAC-SHA1 实现阿里云 OSS V1 签名认证，不依赖官方 SDK
- **环境变量覆盖**：所有配置项均支持环境变量覆盖，方便 CI/CD 集成

---

## 安装

```bash
cargo build --release
cp target/release/nuwax-ossutil /usr/local/bin/
```

---

## 配置

### 方式一：命令行配置（推荐）

```bash
nuwax-ossutil config \
  --endpoint oss-cn-hangzhou.aliyuncs.com \
  --key-id YOUR_ACCESS_KEY_ID \
  --key-secret YOUR_ACCESS_KEY_SECRET \
  --bucket YOUR_BUCKET_NAME
```

配置文件保存路径：`~/.config/nuwax-ossutil.toml`

完整配置示例（含可选字段）：

```toml
endpoint = "oss-cn-hangzhou.aliyuncs.com"
bucket_name = "my-bucket"
access_key_id = "LTAI5t..."
access_key_secret = "xxxxx"
cdn_domain = "https://cdn.example.com"
path_prefix = "my-project"
```

### 方式二：环境变量

所有配置项均支持环境变量，优先级高于配置文件：

| 环境变量 | 说明 | 必填 |
|----------|------|------|
| `OSS_ENDPOINT` | OSS Endpoint，如 `oss-cn-hangzhou.aliyuncs.com` | 是 |
| `OSS_BUCKET_NAME` | Bucket 名称 | 是 |
| `OSS_ACCESS_KEY_ID` | Access Key ID | 是 |
| `OSS_ACCESS_KEY_SECRET` | Access Key Secret | 是 |
| `OSS_CDN_DOMAIN` | CDN 域名，用于生成下载链接 | 否 |
| `OSS_PATH_PREFIX` | 路径前缀，自动拼接在远程路径前 | 否 |

```bash
export OSS_ENDPOINT="oss-cn-hangzhou.aliyuncs.com"
export OSS_BUCKET_NAME="my-bucket"
export OSS_ACCESS_KEY_ID="LTAI5t..."
export OSS_ACCESS_KEY_SECRET="xxxxx"
```

---

## 使用方式

### 上传文件（Docker 模式）

自动生成 `docker/{timestamp}/{filename}` 路径，支持多文件并发上传，大文件自动分片。

```bash
# 上传单个文件
nuwax-ossutil upload-docker -f app.tar.gz

# 上传多个文件
nuwax-ossutil upload-docker -f app.tar.gz -f config.json -f image.zip

# 使用 shell 通配符
nuwax-ossutil upload-docker -f ./dist/*
```

输出示例：

```
📤 开始上传 3 个文件到 docker/20260525143012/

  [1/3] app.tar.gz (12.5 MB) ... ✅ 成功
         下载: https://cdn.example.com/docker/20260525143012/app.tar.gz
  [2/3] config.json (2.1 KB) ... ✅ 成功
         下载: https://cdn.example.com/docker/20260525143012/config.json
  [3/3] image.zip (45.3 MB) ... ✅ 成功 (分片上传)
         下载: https://cdn.example.com/docker/20260525143012/image.zip

📊 上传完成: 成功 3/3
```

**分片策略**：

| 文件大小 | 上传方式 | 说明 |
|----------|----------|------|
| < 5MB | 简单上传（PUT） | 整个文件读入内存，单次 PUT 请求 |
| ≥ 5MB | 分片上传 | 10MB/片，最多 3 片并发，失败自动 Abort |

### 上传文件（自定义路径）

```bash
nuwax-ossutil upload -f ./local/file.zip -r custom/path/file.zip
```

### 列出文件

```bash
# 列出所有文件
nuwax-ossutil list

# 按前缀过滤
nuwax-ossutil list -p docker/20260525
```

### 删除文件

```bash
nuwax-ossutil rm -r docker/20260525143012/app.tar.gz
```

---

## 命令参考

```
nuwax-ossutil

命令:
  config         配置阿里云 OSS 凭证
  upload         上传文件到 OSS（自定义远程路径）
  upload-docker  上传 Docker 文件到 OSS（自动生成 docker/{timestamp}/ 路径）
  list           列出 OSS 中的文件
  rm             删除 OSS 中的文件
  help           打印帮助信息

选项:
  -h, --help     打印帮助
  -V, --version  打印版本号
```

---

## 项目结构

```
src/
├── main.rs                  # CLI 入口，clap 命令定义
├── config.rs                # 配置加载与持久化
├── commands/
│   ├── mod.rs               # 命令模块导出
│   ├── upload.rs            # upload 命令实现
│   ├── upload_docker.rs     # upload-docker 命令实现
│   ├── list.rs              # list 命令实现
│   └── delete.rs            # rm 命令实现
└── oss/
    ├── mod.rs               # OSS 模块导出
    ├── client.rs            # OssClient：签名、上传、列表、删除
    └── mime.rs              # MIME 类型推断
```

---

## 技术细节

### OSS V1 签名

```
StringToSign = HTTP-Verb + "\n"
             + Content-MD5 + "\n"
             + Content-Type + "\n"
             + Date + "\n"
             + CanonicalizedResource

Authorization = "OSS " + AccessKeyId + ":" + Base64(HMAC-SHA1(AccessKeySecret, StringToSign))
```

其中 `CanonicalizedResource` 格式为 `/{BucketName}/{ObjectKey}`，分片上传的子资源参数（`?uploads`、`?partNumber=N&uploadId=X`）也包含在签名路径中。

### 分片上传流程

```
1. POST /{key}?uploads              → 获取 UploadId
2. PUT  /{key}?partNumber=N&uploadId=X  → 上传分片，获取 ETag（并发，最多 3 片）
3. POST /{key}?uploadId=X           → 提交 CompleteMultipartUpload XML
   （失败时）DELETE /{key}?uploadId=X  → Abort，清理已上传分片
```

### 下载链接生成

```
优先使用 CDN 域名：{cdn_domain}/{remote_path}
否则使用 OSS 直链：https://{bucket}.{endpoint}/{remote_path}
```

---

## 依赖

| 依赖 | 版本 | 用途 |
|------|------|------|
| `clap` | 4.6 | CLI 参数解析 |
| `tokio` | 1.x | 异步运行时 |
| `reqwest` | 0.13 | HTTP 客户端 |
| `hmac` | 0.13 | HMAC-SHA1 签名 |
| `sha1` | 0.11 | SHA1 哈希 |
| `base64` | 0.22 | Base64 编码 |
| `serde` | 1.x | 序列化/反序列化 |
| `toml` | 1.x | TOML 配置文件解析 |
| `anyhow` | 1.x | 错误处理 |
| `chrono` | 0.4 | 时间处理与 GMT 日期格式化 |
| `futures` | 0.3 | 并发 Future 管理 |
| `quick-xml` | 0.40 | XML 响应解析 |
| `indicatif` | 0.18 | 进度条显示 |
| `percent-encoding` | 2.x | URL 编码 |
