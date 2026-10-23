# Strict release uploads

The normal `upload` and `upload-docker` interfaces remain compatible. Release
automation opts into the strict path explicitly:

```sh
nuwax-ossutil capabilities
nuwax-ossutil check-bucket --expected-bucket nuwa-packages \
  --expected-endpoint oss-rg-china-mainland.aliyuncs.com
nuwax-ossutil upload --remote beta-release-validation/UTC_TIMESTAMP \
  --forbid-overwrite --verify-crc64 --report-json ./upload-report.json \
  --expected-bucket nuwa-packages \
  --expected-endpoint oss-rg-china-mainland.aliyuncs.com ./candidate.zip
nuwax-ossutil stat --remote beta-release-validation/UTC_TIMESTAMP/candidate.zip
```

Strict upload accepts explicit regular files only; directories, symlinks,
duplicate basenames, existing report files and `--name` are rejected. The two
verification flags, expected bucket/endpoint and report path are required as a
group. It rejects a configured path prefix, non-HTTPS endpoints, and bucket
versioning that is enabled, suspended, unknown or unreadable.

Small PUT, multipart initiation and completion carry
`x-oss-forbid-overwrite: true`. Existing objects fail with the original OSS error.
The final full-file CRC64 and size are compared against signed HEAD metadata;
ETag is not treated as a file hash. Local SHA256 is included in the report.

Strict checkpoints live beside the selected candidate, using a separate protocol
and OS file lock. They bind bucket, endpoint, object key, canonical local path,
file size, SHA256 and CRC64. Changed identities are rejected rather than silently
restarting another operation. Parts run serially with bounded buffers and retries;
this prioritizes inspectable recovery over the legacy path's parallel scheduling.
Checkpoint files are removed only after completion and metadata validation.

On completion timeout, keep the checkpoint and inspect the original UploadId and
object. Do not blindly start a different operation. If OSS already committed the
object but the response was lost, the next completion attempt may return an error;
the tool does not reinterpret that error as success. Reports contain only fully
verified files and are updated after each successful file. Partial directories
are not automatically deleted.

`cargo test --offline` includes local HTTP transport tests for protected PUT,
multipart completion failure/resume, overwrite rejection, and CRC mismatch.
These tests require permission to listen on 127.0.0.1. Strict upload currently
requires macOS or Linux; the legacy path remains available on other platforms.

Real validation on 2026-10-23 uploaded a 7,200,000-byte multipart probe and a
47-byte simple probe under `beta-release-validation/20261023081024/` in
`nuwa-packages`. Both CRC64 values matched OSS; a repeated multipart probe was
rejected with `FileAlreadyExists` and exit code 1. No version pointers were changed.
Version 0.1.8 is distributed through the existing GitHub release-tag/npm pipeline.
Verification uses isolated npm installation prefixes, without replacing global binaries.

Corrupt or unreadable checkpoints fail closed rather than creating a new UploadId.
Reports are reserved with exclusive creation before any object upload; concurrent
commands cannot replace an existing receipt. Verified requests have 30-second
connection and 300-second per-request timeouts and never follow redirects.
