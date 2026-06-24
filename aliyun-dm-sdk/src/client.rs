use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;
use time::{OffsetDateTime, macros::format_description};
use uuid::Uuid;

use crate::{
    error::{ApiError, DirectMailError},
    signer,
    types::{ApiVersion, SingleSendMailRequest, SingleSendMailResponse},
};

type Clock = Arc<dyn Fn() -> String + Send + Sync>;
type NonceGen = Arc<dyn Fn() -> String + Send + Sync>;
type Sleeper = Arc<dyn Fn(Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

const DEFAULT_ENDPOINT: &str = "dm.aliyuncs.com";

#[derive(Clone)]
pub struct DirectMailClient {
    http: reqwest::Client,
    access_key_id: String,
    access_key_secret: String,
    endpoint: String,
    api_version: ApiVersion,
    clock: Clock,
    nonce_gen: NonceGen,
    retry: RetryConfig,
    sleeper: Sleeper,
}

impl fmt::Debug for DirectMailClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirectMailClient")
            .field("endpoint", &self.endpoint)
            .field("access_key_id", &self.access_key_id)
            .field("access_key_secret", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_attempts: usize,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
        }
    }
}

pub struct DirectMailClientBuilder {
    http: reqwest::Client,
    access_key_id: String,
    access_key_secret: String,
    endpoint: String,
    api_version: ApiVersion,
    clock: Clock,
    nonce_gen: NonceGen,
    retry: RetryConfig,
    sleeper: Sleeper,
}

impl DirectMailClient {
    pub fn new(access_key_id: impl Into<String>, access_key_secret: impl Into<String>) -> Self {
        Self::builder(access_key_id, access_key_secret).build()
    }

    pub fn builder(
        access_key_id: impl Into<String>,
        access_key_secret: impl Into<String>,
    ) -> DirectMailClientBuilder {
        DirectMailClientBuilder {
            http: reqwest::Client::new(),
            access_key_id: access_key_id.into(),
            access_key_secret: access_key_secret.into(),
            endpoint: DEFAULT_ENDPOINT.to_owned(),
            api_version: ApiVersion::default(),
            clock: Arc::new(default_timestamp),
            nonce_gen: Arc::new(|| Uuid::new_v4().to_string()),
            retry: RetryConfig::default(),
            sleeper: Arc::new(|duration| Box::pin(tokio::time::sleep(duration))),
        }
    }

    /// Formats the DirectMail endpoint host for a region. This is a pure formatter, not a
    /// supported-region validator: DirectMail is not available in every region (e.g. there is
    /// no `us-west-1` endpoint), so callers should confirm regional availability separately.
    pub fn endpoint_for_region(region: &str) -> String {
        match region {
            "" | "cn-hangzhou" => DEFAULT_ENDPOINT.to_owned(),
            region => format!("dm.{region}.aliyuncs.com"),
        }
    }

    pub async fn single_send_mail(
        &self,
        request: SingleSendMailRequest,
    ) -> Result<SingleSendMailResponse, DirectMailError> {
        request.validate()?;

        let max_attempts = self.retry.max_attempts;
        let mut last_error: Option<DirectMailError> = None;

        for attempt in 1..=max_attempts {
            let params = self.request_params(&request);
            // Aliyun RPC: parameters travel as an application/x-www-form-urlencoded POST body
            // (matches the official Go SDK: Method=POST, ReqBodyType=formData, Pathname="/").
            // The signed query string is itself a valid form body.
            let body = signer::signed_query("POST", &params, &self.access_key_secret);
            let url = format!("{}/", self.base_url());

            let result = match self
                .http
                .post(&url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(body)
                .send()
                .await
            {
                Ok(response) => self.parse_response(response).await,
                Err(error) => Err(DirectMailError::Http(error)),
            };

            match result {
                Ok(response) => return Ok(response),
                Err(error) if !error.is_retryable() => return Err(error),
                Err(error) => last_error = Some(error),
            }

            if attempt < max_attempts {
                (self.sleeper)(self.backoff_delay(attempt)).await;
            }
        }

        Err(DirectMailError::RetryExhausted(Box::new(
            last_error.expect("retry loop runs at least once"),
        )))
    }

    fn request_params(&self, request: &SingleSendMailRequest) -> Vec<(String, String)> {
        let mut params = request.to_query_params(self.api_version);
        params.push(("AccessKeyId".to_owned(), self.access_key_id.clone()));
        params.push(("SignatureMethod".to_owned(), "HMAC-SHA1".to_owned()));
        params.push(("SignatureVersion".to_owned(), "1.0".to_owned()));
        params.push(("SignatureNonce".to_owned(), (self.nonce_gen)()));
        params.push(("Timestamp".to_owned(), (self.clock)()));
        params
    }

    async fn parse_response(
        &self,
        response: reqwest::Response,
    ) -> Result<SingleSendMailResponse, DirectMailError> {
        let status = response.status();
        let status_code = status.as_u16();
        let body = response.text().await?;

        if status.is_success() {
            // Aliyun occasionally returns a 2xx status carrying an error body; detect it via the
            // Code field and surface it as a typed error rather than a decode failure.
            if let Some(api_error) = parse_api_error(status_code, &body) {
                return Err(DirectMailError::Api(api_error));
            }
            return serde_json::from_str::<SingleSendMailResponse>(&body)
                .map_err(DirectMailError::from);
        }

        let api_error = parse_api_error(status_code, &body).unwrap_or_else(|| ApiError {
            status: status_code,
            code: "HttpError".to_owned(),
            // The raw body is intentionally omitted: a non-Aliyun error page could reflect the
            // request URL, which carries recipients/subject/body/Signature in the query string.
            message: format!("non-JSON HTTP response body omitted ({} bytes)", body.len()),
            request_id: None,
        });
        Err(DirectMailError::Api(api_error))
    }

    fn base_url(&self) -> String {
        let endpoint = self.endpoint.trim_end_matches('/');
        if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
            endpoint.to_owned()
        } else {
            format!("https://{endpoint}")
        }
    }

    fn backoff_delay(&self, attempt: usize) -> Duration {
        let base_ms = millis(self.retry.base_delay);
        let max_ms = millis(self.retry.max_delay).max(base_ms);
        let multiplier = 1_u64 << attempt.saturating_sub(1).min(10);
        let exponential_ms = base_ms.saturating_mul(multiplier).min(max_ms);
        let jitter_ms = jitter_millis(exponential_ms / 4);

        Duration::from_millis(exponential_ms.saturating_add(jitter_ms).min(max_ms))
    }
}

impl DirectMailClientBuilder {
    pub fn http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Selects the DirectMail API generation. Defaults to `ApiVersion::V20151123`.
    /// Both generations share identical transport and signature; this only changes the
    /// `Version` request parameter and thus which API generation the server routes to.
    pub fn api_version(mut self, api_version: ApiVersion) -> Self {
        self.api_version = api_version;
        self
    }

    pub fn region(mut self, region: &str) -> Self {
        self.endpoint = DirectMailClient::endpoint_for_region(region);
        self
    }

    pub fn clock<F>(mut self, clock: F) -> Self
    where
        F: Fn() -> String + Send + Sync + 'static,
    {
        self.clock = Arc::new(clock);
        self
    }

    pub fn nonce_gen<F>(mut self, nonce_gen: F) -> Self
    where
        F: Fn() -> String + Send + Sync + 'static,
    {
        self.nonce_gen = Arc::new(nonce_gen);
        self
    }

    pub fn retry_config(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    pub fn sleeper<F, Fut>(mut self, sleeper: F) -> Self
    where
        F: Fn(Duration) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.sleeper = Arc::new(move |duration| Box::pin(sleeper(duration)));
        self
    }

    pub fn build(mut self) -> DirectMailClient {
        self.retry.max_attempts = self.retry.max_attempts.max(1);

        DirectMailClient {
            http: self.http,
            access_key_id: self.access_key_id,
            access_key_secret: self.access_key_secret,
            endpoint: self.endpoint,
            api_version: self.api_version,
            clock: self.clock,
            nonce_gen: self.nonce_gen,
            retry: self.retry,
            sleeper: self.sleeper,
        }
    }
}

#[derive(Debug, Deserialize)]
struct AliyunErrorBody {
    #[serde(rename = "Code", alias = "code")]
    code: String,
    #[serde(rename = "Message", alias = "message")]
    message: String,
    #[serde(rename = "RequestId", alias = "request_id", alias = "requestId")]
    request_id: Option<String>,
}

fn parse_api_error(status: u16, body: &str) -> Option<ApiError> {
    let parsed: AliyunErrorBody = serde_json::from_str(body).ok()?;
    Some(ApiError {
        status,
        code: parsed.code,
        message: parsed.message,
        request_id: parsed.request_id,
    })
}

fn default_timestamp() -> String {
    static FORMAT: &[time::format_description::FormatItem<'_>] =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");

    OffsetDateTime::now_utc()
        .format(FORMAT)
        .expect("static UTC timestamp format is valid")
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

fn jitter_millis(cap: u64) -> u64 {
    if cap == 0 {
        return 0;
    }

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos() as u64)
        .unwrap_or_default();

    nanos % (cap + 1)
}
