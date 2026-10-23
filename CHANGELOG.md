# Changelog

## 0.1.8

- Add opt-in verified uploads with OSS atomic overwrite protection, CRC64/size verification, and JSON receipts.
- Add machine-readable capabilities, bucket versioning checks, and signed object metadata inspection.
- Bind strict resumable uploads to the exact local SHA256, bucket, endpoint, object key, and original upload identity.
- Fail closed on corrupt checkpoints; reserve report files exclusively to prevent concurrent receipt replacement.
- Preserve legacy multipart checkpoints until completion succeeds; keep existing upload commands compatible.
- Bound strict request timeouts and disable redirects. Strict uploads currently support macOS and Linux; legacy uploads remain available on Windows.
- Gate release builds with unit/local HTTP tests and Clippy through cargo-dist's reusable workflow extension.
