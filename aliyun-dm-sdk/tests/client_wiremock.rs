use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use aliyun_dm_sdk::{
    ApiVersion, DirectMailClient, DirectMailError, MailBody, SingleSendMailRequest,
    client::RetryConfig,
    signer,
};
use serde_json::json;
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{method, path},
};

const FIXED_TIMESTAMP: &str = "2026-06-23T12:00:00Z";

#[tokio::test]
async fn builds_signed_post_request_against_mock_server() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "EnvId": "env-123",
            "RequestId": "req-123"
        })))
        .mount(&server)
        .await;

    let client = fixed_client(&server, "nonce-1");
    let response = client.single_send_mail(valid_request()).await.unwrap();
    assert_eq!(response.env_id, "env-123");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);

    let request = &requests[0];
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.url.path(), "/");
    assert_eq!(
        request
            .headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/x-www-form-urlencoded")
    );

    let params = body_pairs(request);
    assert_contains(&params, "Action", "SingleSendMail");
    assert_contains(&params, "Version", "2015-11-23");
    assert_contains(&params, "Format", "JSON");
    assert_contains(&params, "AccessKeyId", "testid");
    assert_contains(&params, "SignatureMethod", "HMAC-SHA1");
    assert_contains(&params, "SignatureVersion", "1.0");
    assert_contains(&params, "SignatureNonce", "nonce-1");
    assert_contains(&params, "Timestamp", FIXED_TIMESTAMP);
    assert_contains(&params, "AccountName", "sender@example.com");
    assert_contains(&params, "ToAddress", "user@example.com");
    assert_contains(&params, "ReplyToAddress", "false");
    assert_contains(&params, "TextBody", "hello");

    let sent_signature = params
        .iter()
        .find(|(key, _)| key == "Signature")
        .map(|(_, value)| value.as_str())
        .unwrap();
    let recomputed = signer::signature("POST", &params, "testsecret");
    assert_eq!(sent_signature, recomputed);
}

#[tokio::test]
async fn api_version_propagates_to_version_param() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "EnvId": "env-v2",
            "RequestId": "req-v2"
        })))
        .mount(&server)
        .await;

    let client = DirectMailClient::builder("testid", "testsecret")
        .endpoint(server.uri())
        .api_version(ApiVersion::V20170622)
        .clock(|| FIXED_TIMESTAMP.to_owned())
        .nonce_gen(|| "nonce-v2".to_owned())
        .retry_config(RetryConfig {
            max_attempts: 1,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        })
        .sleeper(|_| async {})
        .build();

    client.single_send_mail(valid_request()).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let params = body_pairs(&requests[0]);
    assert_contains(&params, "Version", "2017-06-22");
}

#[tokio::test]
async fn retries_throttled_responses_with_fresh_signatures() {
    let server = MockServer::start().await;
    let attempt_count = Arc::new(AtomicUsize::new(0));

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ThrottleThenSuccess {
            attempt_count: Arc::clone(&attempt_count),
        })
        .mount(&server)
        .await;

    let nonce_counter = Arc::new(AtomicUsize::new(0));
    let nonce_counter_for_client = Arc::clone(&nonce_counter);
    let client = DirectMailClient::builder("testid", "testsecret")
        .endpoint(server.uri())
        .clock(|| FIXED_TIMESTAMP.to_owned())
        .nonce_gen(move || {
            let next = nonce_counter_for_client.fetch_add(1, Ordering::SeqCst) + 1;
            format!("nonce-{next}")
        })
        .retry_config(RetryConfig {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        })
        .sleeper(|_| async {})
        .build();

    let response = client.single_send_mail(valid_request()).await.unwrap();

    assert_eq!(response.env_id, "env-after-retry");
    assert_eq!(attempt_count.load(Ordering::SeqCst), 3);

    let requests = server.received_requests().await.unwrap();
    let nonces = requests
        .iter()
        .map(|request| {
            body_pairs(request)
                .into_iter()
                .find(|(key, _)| key == "SignatureNonce")
                .map(|(_, value)| value)
                .unwrap()
        })
        .collect::<Vec<_>>();

    assert_eq!(nonces, vec!["nonce-1", "nonce-2", "nonce-3"]);
}

#[tokio::test]
async fn maps_aliyun_error_json_to_typed_api_error() {
    let cases = [
        (404u16, "InvalidMailAddress.NotFound"),
        (400, "InvalidMailAddressName.Malformed"),
        (400, "InvalidQuota"),
        (400, "InvalidSendMail.Spam"),
        (400, "InvalidSignature"),
    ];

    for (status, code) in cases {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "Code": code,
                "Message": "aliyun rejected the request",
                "RequestId": "req-error"
            })))
            .mount(&server)
            .await;

        let client = fixed_client(&server, "nonce-error");
        let error = client.single_send_mail(valid_request()).await.unwrap_err();

        match error {
            DirectMailError::Api(api) => {
                assert_eq!(api.status, status);
                assert_eq!(api.code, code);
                assert_eq!(api.message, "aliyun rejected the request");
                assert_eq!(api.request_id.as_deref(), Some("req-error"));
            }
            other => panic!("expected Api error for {code}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn retries_non_json_server_error_without_exposing_body() {
    let server = MockServer::start().await;
    let attempt_count = Arc::new(AtomicUsize::new(0));

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(NonJsonServerError {
            attempt_count: Arc::clone(&attempt_count),
        })
        .mount(&server)
        .await;

    let client = DirectMailClient::builder("testid", "testsecret")
        .endpoint(server.uri())
        .clock(|| FIXED_TIMESTAMP.to_owned())
        .nonce_gen(|| "nonce-err".to_owned())
        .retry_config(RetryConfig {
            max_attempts: 2,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        })
        .sleeper(|_| async {})
        .build();

    let error = client.single_send_mail(valid_request()).await.unwrap_err();

    match error {
        DirectMailError::RetryExhausted(inner) => match *inner {
            DirectMailError::Api(api) => {
                assert_eq!(api.status, 500);
                assert_eq!(api.code, "HttpError");
                assert!(api.message.contains("bytes"), "message was: {}", api.message);
                assert!(
                    !api.message.contains("SENSITIVE-REFLECTED-DATA"),
                    "raw body leaked into error message: {}",
                    api.message
                );
            }
            other => panic!("expected Api inside RetryExhausted, got {other:?}"),
        },
        other => panic!("expected RetryExhausted, got {other:?}"),
    }

    assert_eq!(attempt_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
#[ignore = "requires real Aliyun DirectMail credentials and a verified sender account"]
async fn real_smoke_test_env_gated() {
    if std::env::var_os("ALIYUN_DIRECTMAIL_SMOKE").is_none() {
        return;
    }

    let access_key_id = std::env::var("ALIYUN_ACCESS_KEY_ID").unwrap();
    let access_key_secret = std::env::var("ALIYUN_ACCESS_KEY_SECRET").unwrap();
    let account_name = std::env::var("ALIYUN_DIRECTMAIL_ACCOUNT_NAME").unwrap();
    let to_address = std::env::var("ALIYUN_DIRECTMAIL_TO_ADDRESS").unwrap();

    let client = DirectMailClient::builder(access_key_id, access_key_secret).build();
    let request = SingleSendMailRequest::new(
        account_name,
        to_address,
        "DirectMail smoke test",
        MailBody::Text("DirectMail smoke test from aliyun-directmail-smtp-gateway".to_owned()),
    );

    let response = client.single_send_mail(request).await.unwrap();
    assert!(!response.env_id.is_empty());
}

fn fixed_client(server: &MockServer, nonce: &'static str) -> DirectMailClient {
    DirectMailClient::builder("testid", "testsecret")
        .endpoint(server.uri())
        .clock(|| FIXED_TIMESTAMP.to_owned())
        .nonce_gen(move || nonce.to_owned())
        .retry_config(RetryConfig {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        })
        .sleeper(|_| async {})
        .build()
}

fn valid_request() -> SingleSendMailRequest {
    SingleSendMailRequest::new(
        "sender@example.com",
        "user@example.com",
        "subject",
        MailBody::Text("hello".to_owned()),
    )
}

fn body_pairs(request: &Request) -> Vec<(String, String)> {
    // Aliyun RPC requests are sent as application/x-www-form-urlencoded POST bodies.
    url::form_urlencoded::parse(&request.body)
        .into_owned()
        .collect()
}

fn assert_contains(params: &[(String, String)], key: &str, value: &str) {
    assert!(
        params.iter().any(|(k, v)| k == key && v == value),
        "missing query pair {key}={value}; got {params:?}"
    );
}

struct ThrottleThenSuccess {
    attempt_count: Arc<AtomicUsize>,
}

struct NonJsonServerError {
    attempt_count: Arc<AtomicUsize>,
}

impl Respond for NonJsonServerError {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.attempt_count.fetch_add(1, Ordering::SeqCst);
        ResponseTemplate::new(500).set_body_raw(
            "<html><body>gateway error SENSITIVE-REFLECTED-DATA</body></html>".as_bytes(),
            "text/html",
        )
    }
}

impl Respond for ThrottleThenSuccess {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let attempt = self.attempt_count.fetch_add(1, Ordering::SeqCst) + 1;

        if attempt <= 2 {
            ResponseTemplate::new(429).set_body_json(json!({
                "Code": "Throttling.User",
                "Message": "too many requests",
                "RequestId": format!("req-throttle-{attempt}")
            }))
        } else {
            ResponseTemplate::new(200).set_body_json(json!({
                "EnvId": "env-after-retry",
                "RequestId": "req-success"
            }))
        }
    }
}
