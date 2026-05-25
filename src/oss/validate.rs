#![allow(dead_code)]

use std::path::Path;

const MIN_BUCKET_NAME_LENGTH: usize = 3;
const MAX_BUCKET_NAME_LENGTH: usize = 63;
const MAX_OBJECT_KEY_LENGTH: usize = 1023;

/// Validate bucket name according to OSS rules:
/// - Length between [3, 63]
/// - Only lowercase letters, digits, and hyphens allowed
/// - Must start and end with a letter or digit (not a hyphen)
pub fn validate_bucket_name(name: &str) -> bool {
    if name.len() < MIN_BUCKET_NAME_LENGTH || name.len() > MAX_BUCKET_NAME_LENGTH {
        return false;
    }
    if name.starts_with('-') || name.ends_with('-') {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Validate object key:
/// - Not empty, max 1023 chars
/// - Must not start or end with `/` or `\`
pub fn validate_object_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_OBJECT_KEY_LENGTH
        && !key.starts_with('/')
        && !key.starts_with('\\')
        && !key.ends_with('/')
        && !key.ends_with('\\')
}

/// Validate local file path exists and is a regular file
pub fn validate_file_path(path: &str) -> bool {
    let p = Path::new(path);
    p.exists() && p.is_file()
}

/// Extract region from OSS endpoint.
/// e.g. "oss-cn-hangzhou.aliyuncs.com" → "cn-hangzhou"
pub fn get_region_from_endpoint(endpoint: &str) -> Option<String> {
    let clean = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .unwrap_or(endpoint);

    clean
        .find('.')
        .map(|idx| clean[..idx].replace("oss-", ""))
}

/// Validate OSS endpoint format
pub fn validate_endpoint(endpoint: &str) -> bool {
    let clean = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .unwrap_or(endpoint);

    !clean.is_empty()
        && clean.contains('.')
        && !clean.starts_with('.')
        && !clean.ends_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_bucket_name() {
        assert!(validate_bucket_name("my-bucket"));
        assert!(validate_bucket_name("abc"));
        assert!(validate_bucket_name("test-bucket-123"));
        assert!(!validate_bucket_name("ab")); // too short
        assert!(!validate_bucket_name("-bucket")); // starts with hyphen
        assert!(!validate_bucket_name("bucket-")); // ends with hyphen
        assert!(!validate_bucket_name("My-Bucket")); // uppercase
        assert!(!validate_bucket_name("my_bucket")); // underscore
    }

    #[test]
    fn test_validate_object_key() {
        assert!(validate_object_key("file.txt"));
        assert!(validate_object_key("dir/file.txt"));
        assert!(validate_object_key("a/b/c/d.zip"));
        assert!(!validate_object_key("")); // empty
        assert!(!validate_object_key("/file.txt")); // starts with /
        assert!(!validate_object_key("dir/")); // ends with /
    }

    #[test]
    fn test_get_region_from_endpoint() {
        assert_eq!(
            get_region_from_endpoint("oss-cn-hangzhou.aliyuncs.com"),
            Some("cn-hangzhou".to_string())
        );
        assert_eq!(
            get_region_from_endpoint("oss-cn-beijing.aliyuncs.com"),
            Some("cn-beijing".to_string())
        );
        assert_eq!(
            get_region_from_endpoint("https://oss-ap-southeast-1.aliyuncs.com"),
            Some("ap-southeast-1".to_string())
        );
        assert_eq!(get_region_from_endpoint("no-dot"), None);
    }

    #[test]
    fn test_validate_endpoint() {
        assert!(validate_endpoint("oss-cn-hangzhou.aliyuncs.com"));
        assert!(validate_endpoint("https://oss-cn-hangzhou.aliyuncs.com"));
        assert!(!validate_endpoint(""));
        assert!(!validate_endpoint("no-dot"));
        assert!(!validate_endpoint(".starts-with-dot.com"));
    }
}
