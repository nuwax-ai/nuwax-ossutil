/// Guess MIME content type from file path extension
pub fn guess_content_type(path: &str) -> String {
    let lower = path.to_lowercase();
    if lower.ends_with(".zip") {
        "application/zip"
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        "application/gzip"
    } else if lower.ends_with(".tar") {
        "application/x-tar"
    } else if lower.ends_with(".gz") {
        "application/gzip"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".xml") {
        "application/xml"
    } else if lower.ends_with(".html") || lower.ends_with(".htm") {
        "text/html"
    } else if lower.ends_with(".css") {
        "text/css"
    } else if lower.ends_with(".js") {
        "application/javascript"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if lower.ends_with(".pdf") {
        "application/pdf"
    } else if lower.ends_with(".exe") {
        "application/x-msdownload"
    } else if lower.ends_with(".dmg") {
        "application/x-apple-diskimage"
    } else if lower.ends_with(".deb") {
        "application/x-debian-package"
    } else if lower.ends_with(".rpm") {
        "application/x-rpm"
    } else {
        "application/octet-stream"
    }
    .to_string()
}
