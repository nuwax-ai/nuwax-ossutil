use quick_xml::events::Event;
use quick_xml::name::QName;
use quick_xml::Reader;
use std::fmt;
use thiserror::Error;

/// OSS API error response parsed from XML body.
///
/// ```xml
/// <?xml version="1.0" ?>
/// <Error>
///   <Code>MalformedXML</Code>
///   <Message>The XML you provided was not well-formed.</Message>
///   <RequestId>57ABD896CCB80C366955****</RequestId>
///   <HostId>oss-cn-hangzhou.aliyuncs.com</HostId>
///   <EC>0031-00000001</EC>
///   <RecommendDoc>https://api.aliyun.com/troubleshoot?q=0031-00000001</RecommendDoc>
/// </Error>
/// ```
#[allow(dead_code)]
#[derive(Debug, Default, Clone)]
pub struct OssApiError {
    pub code: String,
    pub message: String,
    pub request_id: String,
    pub host_id: String,
    pub ec: String,
    pub recommend_doc: String,
}

impl OssApiError {
    #[allow(dead_code)]
    pub fn from_xml(xml_content: &str) -> Self {
        let mut reader = Reader::from_str(xml_content);
        let mut result = Self::default();
        let mut current_tag = String::new();

        loop {
            match reader.read_event() {
                Ok(Event::Start(ref e)) => {
                    current_tag = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                }
                Ok(Event::Text(ref e)) => {
                    let text = String::from_utf8_lossy(e).to_string();
                    match current_tag.as_str() {
                        "Code" => result.code = text,
                        "Message" => result.message = text,
                        "RequestId" => result.request_id = text,
                        "HostId" => result.host_id = text,
                        "EC" => result.ec = text,
                        "RecommendDoc" => result.recommend_doc = text,
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) if e.name() == QName(b"Error") => break,
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
        }

        result
    }
}

impl fmt::Display for OssApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "OSS API Error [{}]: {} (RequestId: {})",
            self.code, self.message, self.request_id
        )
    }
}

/// Error types for OSS operations.
#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum OssError {
    #[error("OSS API error: {0}")]
    Api(Box<OssApiError>),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("invalid header name: {0}")]
    InvalidHeaderName(#[from] reqwest::header::InvalidHeaderName),

    #[error("invalid header value: {0}")]
    InvalidHeaderValue(#[from] reqwest::header::InvalidHeaderValue),

    #[error("URL error: {0}")]
    UrlError(String),

    #[error("invalid bucket name: {0}")]
    InvalidBucketName(String),

    #[error("invalid object key: {0}")]
    InvalidObjectKey(String),

    #[error("invalid file path: {0}")]
    InvalidFilePath(String),

    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),

    #[error("config error: {0}")]
    ConfigError(String),

    #[error("XML parse error: {0}")]
    XmlParse(String),

    #[error("{0}")]
    Other(String),
}

impl From<OssApiError> for OssError {
    fn from(err: OssApiError) -> Self {
        OssError::Api(Box::new(err))
    }
}

impl From<quick_xml::Error> for OssError {
    fn from(err: quick_xml::Error) -> Self {
        OssError::XmlParse(err.to_string())
    }
}

impl From<anyhow::Error> for OssError {
    fn from(err: anyhow::Error) -> Self {
        OssError::Other(err.to_string())
    }
}
