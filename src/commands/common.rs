use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

/// Result of a single file upload task.
pub struct UploadResult {
    pub filename: String,
    pub file_size: u64,
    pub is_multipart: bool,
    pub download_url: Option<String>,
    pub error: Option<String>,
}

/// Format file size in human-readable form.
pub fn format_file_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Hidden/system files that should always be skipped during directory expansion.
const SKIP_FILES: &[&str] = &[".DS_Store", "Thumbs.db"];

/// Expand directories in the input list: replace each directory with its direct children (files only).
/// - If `skip_dir` is true, sub-directories are silently skipped.
/// - If `skip_dir` is false, encountering a sub-directory returns an error immediately (Fail Fast).
/// - Non-directory paths are kept as-is (existence validation happens later).
/// - Hidden/system files (.DS_Store, Thumbs.db) inside directories are always skipped.
pub fn expand_paths(paths: &[PathBuf], skip_dir: bool) -> Result<Vec<PathBuf>> {
    let mut expanded = Vec::new();
    for path in paths {
        if path.is_dir() {
            let entries = std::fs::read_dir(path)
                .map_err(|e| anyhow!("无法读取目录 {}: {}", path.display(), e))?;
            for entry in entries {
                let entry = entry.map_err(|e| anyhow!("读取目录条目失败: {}", e))?;
                let p = entry.path();
                if p.is_dir() {
                    if skip_dir {
                        eprintln!("⚠️  跳过目录: {}", p.display());
                    } else {
                        return Err(anyhow!("不是文件: {}", p.display()));
                    }
                } else if is_skip_file(&p) {
                    // Silently skip hidden/system files
                } else {
                    expanded.push(p);
                }
            }
        } else {
            expanded.push(path.clone());
        }
    }
    Ok(expanded)
}

/// Check if a file should be skipped (hidden/system files like .DS_Store).
fn is_skip_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| SKIP_FILES.contains(&n))
        .unwrap_or(false)
}
