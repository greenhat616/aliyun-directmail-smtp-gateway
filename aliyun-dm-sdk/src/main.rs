use aliyun_dm_sdk::{DirectMailClient, MailBody, SingleSendMailRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("ALIYUN_DIRECTMAIL_EXAMPLE").is_none() {
        eprintln!("Set ALIYUN_DIRECTMAIL_EXAMPLE=1 to run the DirectMail example.");
        return Ok(());
    }

    let access_key_id = std::env::var("ALIYUN_ACCESS_KEY_ID")?;
    let access_key_secret = std::env::var("ALIYUN_ACCESS_KEY_SECRET")?;
    let account_name = std::env::var("ALIYUN_DIRECTMAIL_ACCOUNT_NAME")?;
    let to_address = std::env::var("ALIYUN_DIRECTMAIL_TO_ADDRESS")?;
    let subject =
        std::env::var("ALIYUN_DIRECTMAIL_SUBJECT").unwrap_or_else(|_| "DirectMail test".to_owned());
    let body = std::env::var("ALIYUN_DIRECTMAIL_TEXT_BODY")
        .unwrap_or_else(|_| "Hello from Rust".to_owned());
    let endpoint = std::env::var("ALIYUN_DIRECTMAIL_ENDPOINT").ok();

    let mut builder = DirectMailClient::builder(access_key_id, access_key_secret);
    if let Some(endpoint) = endpoint {
        builder = builder.endpoint(endpoint);
    }

    let client = builder.build();
    let request =
        SingleSendMailRequest::new(account_name, to_address, subject, MailBody::Text(body));

    let response = client.single_send_mail(request).await?;
    println!("{}", response.env_id);

    Ok(())
}
