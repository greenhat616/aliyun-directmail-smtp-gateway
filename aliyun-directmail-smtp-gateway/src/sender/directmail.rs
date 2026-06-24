use async_trait::async_trait;

use aliyun_dm_sdk::{DirectMailClient, DirectMailError};

use crate::config::DirectMailConfig;
use crate::mail::OutgoingMail;
use crate::sender::{MailSender, SendError, SendOutcome};

/// [`MailSender`] backed by Aliyun DirectMail. Wraps a [`DirectMailClient`] and carries the
/// verified sender account plus per-send flags from configuration.
pub struct DirectMailSender {
    client: DirectMailClient,
    account_name: String,
    address_type: u8,
    reply_to_address: bool,
}

impl DirectMailSender {
    pub fn new(config: DirectMailConfig) -> Self {
        let mut builder =
            DirectMailClient::builder(config.access_key_id, config.access_key_secret);
        if let Some(endpoint) = config.endpoint {
            builder = builder.endpoint(endpoint);
        } else if let Some(region) = config.region {
            builder = builder.region(&region);
        }

        Self {
            client: builder.build(),
            account_name: config.account_name,
            address_type: config.address_type,
            reply_to_address: config.reply_to_address,
        }
    }
}

#[async_trait]
impl MailSender for DirectMailSender {
    async fn send(&self, msg: OutgoingMail) -> Result<SendOutcome, SendError> {
        let request =
            msg.to_directmail_request(&self.account_name, self.address_type, self.reply_to_address);

        match self.client.single_send_mail(request).await {
            Ok(response) => Ok(SendOutcome {
                provider_id: Some(response.env_id),
                request_id: Some(response.request_id),
            }),
            // Validation errors are deterministic content rejections: never retry.
            Err(error @ DirectMailError::Validation(_)) => {
                Err(SendError::Permanent(error.to_string()))
            }
            Err(error) => Err(SendError::Provider(error)),
        }
    }
}
