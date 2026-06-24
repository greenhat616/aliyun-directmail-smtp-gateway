use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

/// Runtime configuration for the gateway. Construct via [`GatewayConfig::from_env`] in the binary,
/// or build directly in tests.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub bind_addr: SocketAddr,
    pub hostname: String,
    pub max_message_bytes: usize,
    pub max_recipients: usize,
    pub max_connections: usize,
    pub read_timeout: Duration,
    pub send_timeout: Duration,
    pub sender: SenderConfig,
}

/// Which delivery backend the gateway uses.
#[derive(Debug, Clone)]
pub enum SenderConfig {
    DirectMail(DirectMailConfig),
    InMemory,
}

#[derive(Clone)]
pub struct DirectMailConfig {
    /// Verified DirectMail sender account. Independent of the SMTP envelope/header `From`.
    pub account_name: String,
    pub access_key_id: String,
    pub access_key_secret: String,
    /// Explicit endpoint host; takes precedence over `region` when set.
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub address_type: u8,
    pub reply_to_address: bool,
}

impl fmt::Debug for DirectMailConfig {
    /// Redacts `access_key_secret` so credentials never reach logs or diagnostics.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirectMailConfig")
            .field("account_name", &self.account_name)
            .field("access_key_id", &self.access_key_id)
            .field("access_key_secret", &"<redacted>")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("address_type", &self.address_type)
            .field("reply_to_address", &self.reply_to_address)
            .finish()
    }
}

/// Default plaintext bind: loopback only. Wider exposure is the deployer's responsibility since
/// this stage advertises neither AUTH nor STARTTLS.
const DEFAULT_BIND_ADDR: &str = "127.0.0.1:2525";
const DEFAULT_HOSTNAME: &str = "localhost";
const DEFAULT_MAX_MESSAGE_BYTES: usize = 10 * 1024 * 1024;
/// DirectMail accepts at most 100 recipients per `SingleSendMail`.
const DEFAULT_MAX_RECIPIENTS: usize = 100;
/// Bounds concurrently-served connections so memory stays roughly `connections × max_message_bytes`.
const DEFAULT_MAX_CONNECTIONS: usize = 1024;
const DEFAULT_READ_TIMEOUT_SECS: u64 = 300;
/// Session-level ceiling on a single delivery attempt. Must be >= the SDK's worst-case retry budget.
const DEFAULT_SEND_TIMEOUT_SECS: u64 = 30;
const DEFAULT_ADDRESS_TYPE: u8 = 1;

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            bind_addr: DEFAULT_BIND_ADDR.parse().expect("valid default bind addr"),
            hostname: DEFAULT_HOSTNAME.to_owned(),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            max_recipients: DEFAULT_MAX_RECIPIENTS,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            read_timeout: Duration::from_secs(DEFAULT_READ_TIMEOUT_SECS),
            send_timeout: Duration::from_secs(DEFAULT_SEND_TIMEOUT_SECS),
            sender: SenderConfig::InMemory,
        }
    }
}

/// Failure to assemble configuration from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Missing(&'static str),
    Invalid { key: &'static str, detail: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(key) => write!(f, "missing required environment variable {key}"),
            Self::Invalid { key, detail } => write!(f, "invalid value for {key}: {detail}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl GatewayConfig {
    /// Builds configuration from `GATEWAY_*` / `DIRECTMAIL_*` environment variables, falling back
    /// to defaults. When `GATEWAY_SENDER=directmail`, the DirectMail credentials are required.
    pub fn from_env() -> Result<Self, ConfigError> {
        let mut config = Self::default();

        if let Some(bind) = env_opt("GATEWAY_BIND_ADDR") {
            config.bind_addr = bind.parse().map_err(|err: std::net::AddrParseError| {
                ConfigError::Invalid { key: "GATEWAY_BIND_ADDR", detail: err.to_string() }
            })?;
        }
        if let Some(hostname) = env_opt("GATEWAY_HOSTNAME") {
            config.hostname = hostname;
        }
        if let Some(max) = env_parse("GATEWAY_MAX_MESSAGE_BYTES")? {
            config.max_message_bytes = max;
        }
        if let Some(max) = env_parse("GATEWAY_MAX_RECIPIENTS")? {
            config.max_recipients = max;
        }
        if let Some(max) = env_parse("GATEWAY_MAX_CONNECTIONS")? {
            config.max_connections = max;
        }
        if let Some(secs) = env_parse::<u64>("GATEWAY_READ_TIMEOUT_SECS")? {
            config.read_timeout = Duration::from_secs(secs);
        }
        if let Some(secs) = env_parse::<u64>("GATEWAY_SEND_TIMEOUT_SECS")? {
            config.send_timeout = Duration::from_secs(secs);
        }

        // Reject unrecognized sender kinds rather than silently defaulting to in-memory, which
        // would accept mail and drop it instead of delivering via DirectMail.
        let sender_kind = env_opt("GATEWAY_SENDER")
            .unwrap_or_else(|| "in-memory".to_owned())
            .to_ascii_lowercase();
        config.sender = match sender_kind.as_str() {
            "directmail" => SenderConfig::DirectMail(DirectMailConfig::from_env()?),
            "in-memory" | "memory" | "recording" => SenderConfig::InMemory,
            _ => {
                return Err(ConfigError::Invalid {
                    key: "GATEWAY_SENDER",
                    detail: "expected 'directmail' or 'in-memory'".to_owned(),
                });
            }
        };

        config.validate()?;
        Ok(config)
    }

    /// Validates invariants the gateway and DirectMail both rely on. Called by `from_env` and again
    /// at bind time so programmatically-built configs are checked too.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.hostname.trim().is_empty() {
            return Err(ConfigError::Invalid {
                key: "hostname",
                detail: "must not be empty".to_owned(),
            });
        }
        if self.max_message_bytes == 0 {
            return Err(ConfigError::Invalid {
                key: "max_message_bytes",
                detail: "must be greater than zero".to_owned(),
            });
        }
        // DirectMail's SingleSendMail accepts at most 100 recipients per call.
        if self.max_recipients == 0 || self.max_recipients > 100 {
            return Err(ConfigError::Invalid {
                key: "max_recipients",
                detail: "must be between 1 and 100".to_owned(),
            });
        }
        if self.max_connections == 0 || self.max_connections > 100_000 {
            return Err(ConfigError::Invalid {
                key: "max_connections",
                detail: "must be between 1 and 100000".to_owned(),
            });
        }
        if self.read_timeout.is_zero() {
            return Err(ConfigError::Invalid {
                key: "read_timeout",
                detail: "must be greater than zero".to_owned(),
            });
        }
        if self.send_timeout.is_zero() {
            return Err(ConfigError::Invalid {
                key: "send_timeout",
                detail: "must be greater than zero".to_owned(),
            });
        }
        if let SenderConfig::DirectMail(directmail) = &self.sender {
            directmail.validate()?;
        }
        Ok(())
    }

    /// Builds a loopback config bound to `addr` with the in-memory sender, for tests.
    #[doc(hidden)]
    pub fn for_test(addr: SocketAddr) -> Self {
        Self {
            bind_addr: addr,
            hostname: "test.local".to_owned(),
            read_timeout: Duration::from_secs(5),
            ..Self::default()
        }
    }
}

impl DirectMailConfig {
    fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            account_name: env_required("DIRECTMAIL_ACCOUNT_NAME")?,
            access_key_id: env_required("DIRECTMAIL_ACCESS_KEY_ID")?,
            access_key_secret: env_required("DIRECTMAIL_ACCESS_KEY_SECRET")?,
            endpoint: env_opt("DIRECTMAIL_ENDPOINT"),
            region: env_opt("DIRECTMAIL_REGION"),
            address_type: env_parse("DIRECTMAIL_ADDRESS_TYPE")?.unwrap_or(DEFAULT_ADDRESS_TYPE),
            reply_to_address: env_parse("DIRECTMAIL_REPLY_TO_ADDRESS")?.unwrap_or(false),
        })
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for (key, value) in [
            ("DIRECTMAIL_ACCOUNT_NAME", &self.account_name),
            ("DIRECTMAIL_ACCESS_KEY_ID", &self.access_key_id),
            ("DIRECTMAIL_ACCESS_KEY_SECRET", &self.access_key_secret),
        ] {
            if value.trim().is_empty() {
                return Err(ConfigError::Invalid { key, detail: "must not be empty".to_owned() });
            }
        }
        Ok(())
    }
}

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.trim().is_empty())
}

fn env_required(key: &'static str) -> Result<String, ConfigError> {
    env_opt(key).ok_or(ConfigError::Missing(key))
}

fn env_parse<T>(key: &'static str) -> Result<Option<T>, ConfigError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match env_opt(key) {
        Some(value) => value
            .parse::<T>()
            .map(Some)
            .map_err(|err| ConfigError::Invalid { key, detail: err.to_string() }),
        None => Ok(None),
    }
}
