use std::sync::Arc;

use aliyun_directmail_smtp_gateway::{
    run, DirectMailSender, GatewayConfig, GatewayError, MailSender, RecordingSender, SenderConfig,
};
use tracing::warn;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), GatewayError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let config = GatewayConfig::from_env()?;

    let sender: Arc<dyn MailSender> = match config.sender.clone() {
        SenderConfig::DirectMail(directmail) => Arc::new(DirectMailSender::new(directmail)),
        SenderConfig::InMemory => {
            warn!("using in-memory sender; accepted messages are recorded locally, not delivered");
            Arc::new(RecordingSender::new())
        }
    };

    run(config, sender).await
}
