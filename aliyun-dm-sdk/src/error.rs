use thiserror::Error;

#[derive(Debug, Error)]
pub enum DirectMailError {
    #[error("validation error: {0}")]
    Validation(String),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("directmail api error: {0}")]
    Api(ApiError),

    #[error("retry attempts exhausted: {0}")]
    RetryExhausted(Box<Self>),

    #[error("decode error: {0}")]
    Decode(#[from] serde_json::Error),
}

impl DirectMailError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Api(error) => error.is_retryable(),
            // Only connection-level errors are retried by default: they occur before the request
            // reaches Aliyun, so retrying cannot duplicate a send. Read timeouts are deliberately
            // NOT retried here because the server may already have accepted and sent the mail.
            Self::Http(error) => error.is_connect(),
            Self::RetryExhausted(error) => error.is_retryable(),
            Self::Validation(_) | Self::Decode(_) => false,
        }
    }
}

#[derive(Debug, Clone, Error)]
#[error("status={status} code={code} message={message}")]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub request_id: Option<String>,
}

impl ApiError {
    pub fn is_retryable(&self) -> bool {
        self.status == 429 || (500..=599).contains(&self.status) || self.code.contains("Throttl")
    }
}
