use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aliyun_directmail_smtp_gateway::{
    GatewayConfig, MailSender, OutgoingMail, ParsedBody, RecordingSender, SendError, SendOutcome,
    SmtpServer,
};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

/// Starts a gateway on an ephemeral loopback port backed by a [`RecordingSender`]. Returns the
/// bound address, a handle to the sender for assertions, and the serving task.
async fn start_server(max_message_bytes: usize) -> (SocketAddr, Arc<RecordingSender>, JoinHandle<()>) {
    let sender = Arc::new(RecordingSender::new());
    let mut config = GatewayConfig::for_test("127.0.0.1:0".parse().unwrap());
    config.max_message_bytes = max_message_bytes;

    let (addr, handle) = start_configured_server(config, sender.clone()).await;
    (addr, sender, handle)
}

/// Starts a gateway from an explicit config and sender, for tests that exercise connection caps or
/// delivery timeouts rather than the default recording path.
async fn start_configured_server(
    config: GatewayConfig,
    sender: Arc<dyn MailSender>,
) -> (SocketAddr, JoinHandle<()>) {
    let server = SmtpServer::bind(config, sender).await.unwrap();
    let addr = server.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = server.serve().await;
    });

    (addr, handle)
}

async fn connect(addr: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    assert_code(&read_reply(&mut stream).await, 220);
    stream
}

async fn command(stream: &mut TcpStream, command: &str, expected_code: u16) -> String {
    stream.write_all(command.as_bytes()).await.unwrap();
    let reply = read_reply(stream).await;
    assert_code(&reply, expected_code);
    reply
}

/// Reads a full (possibly multi-line) SMTP reply: lines continue while the 4th byte is `-`.
async fn read_reply(stream: &mut TcpStream) -> String {
    let mut response = String::new();
    loop {
        let line = read_line(stream).await;
        let last = line.as_bytes().get(3).copied() != Some(b'-');
        response.push_str(&line);
        if last {
            return response;
        }
    }
}

async fn read_line(stream: &mut TcpStream) -> String {
    let mut line = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte).await.unwrap();
        line.push(byte[0]);
        if line.ends_with(b"\r\n") {
            return String::from_utf8(line).unwrap();
        }
    }
}

fn assert_code(reply: &str, code: u16) {
    assert!(reply.starts_with(&code.to_string()), "expected SMTP {code}, got {reply:?}");
}

fn text_body(body: &ParsedBody) -> &str {
    match body {
        ParsedBody::Text(body) => body,
        other => panic!("expected text body, got {other:?}"),
    }
}

fn html_body(body: &ParsedBody) -> &str {
    match body {
        ParsedBody::Html(body) => body,
        other => panic!("expected HTML body, got {other:?}"),
    }
}

/// Submits a single message whose `DATA` payload is exactly `raw_message` (the terminating
/// `<CRLF>.<CRLF>` is appended here), asserts the final reply code, and returns the sender for
/// content assertions.
async fn submit_raw_message(raw_message: &str, expected_code: u16) -> Arc<RecordingSender> {
    let (addr, sender, handle) = start_server(1024 * 1024).await;
    let mut stream = connect(addr).await;

    command(&mut stream, "EHLO client.example\r\n", 250).await;
    command(&mut stream, "MAIL FROM:<sender@example.com>\r\n", 250).await;
    command(&mut stream, "RCPT TO:<a@example.com>\r\n", 250).await;
    command(&mut stream, "DATA\r\n", 354).await;
    stream
        .write_all(format!("{raw_message}\r\n.\r\n").as_bytes())
        .await
        .unwrap();
    assert_code(&read_reply(&mut stream).await, expected_code);

    handle.abort();
    sender
}

#[tokio::test]
async fn smtp_happy_path_records_envelope_subject_and_text_body() {
    let (addr, sender, handle) = start_server(1024 * 1024).await;
    let mut stream = connect(addr).await;

    command(&mut stream, "EHLO client.example\r\n", 250).await;
    command(&mut stream, "MAIL FROM:<alice@example.com>\r\n", 250).await;
    command(&mut stream, "RCPT TO:<bob@example.com>\r\n", 250).await;
    command(&mut stream, "DATA\r\n", 354).await;
    stream
        .write_all(
            b"From: Alice <alice@example.com>\r\n\
Subject: Hello world\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Body text\r\n\
.\r\n",
        )
        .await
        .unwrap();
    assert_code(&read_reply(&mut stream).await, 250);

    let messages = sender.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].envelope_from, "alice@example.com");
    assert_eq!(messages[0].recipients, vec!["bob@example.com"]);
    assert_eq!(messages[0].parsed.subject, "Hello world");
    assert_eq!(messages[0].parsed.from_alias.as_deref(), Some("Alice"));
    assert!(text_body(&messages[0].parsed.body).contains("Body text"));

    handle.abort();
}

#[tokio::test]
async fn smtp_multiple_rcpt_maps_all_recipients() {
    let (addr, sender, handle) = start_server(1024 * 1024).await;
    let mut stream = connect(addr).await;

    command(&mut stream, "EHLO client.example\r\n", 250).await;
    command(&mut stream, "MAIL FROM:<sender@example.com>\r\n", 250).await;
    command(&mut stream, "RCPT TO:<a@example.com>\r\n", 250).await;
    command(&mut stream, "RCPT TO:<b@example.com>\r\n", 250).await;
    command(&mut stream, "DATA\r\n", 354).await;
    stream
        .write_all(b"Subject: Multi\r\nContent-Type: text/plain\r\n\r\nhello\r\n.\r\n")
        .await
        .unwrap();
    assert_code(&read_reply(&mut stream).await, 250);

    let messages = sender.messages();
    assert_eq!(
        messages[0].recipients,
        vec!["a@example.com".to_owned(), "b@example.com".to_owned()]
    );

    handle.abort();
}

#[tokio::test]
async fn smtp_data_dot_stuffing_is_unescaped() {
    let (addr, sender, handle) = start_server(1024 * 1024).await;
    let mut stream = connect(addr).await;

    command(&mut stream, "EHLO client.example\r\n", 250).await;
    command(&mut stream, "MAIL FROM:<sender@example.com>\r\n", 250).await;
    command(&mut stream, "RCPT TO:<a@example.com>\r\n", 250).await;
    command(&mut stream, "DATA\r\n", 354).await;
    stream
        .write_all(b"Subject: Dots\r\nContent-Type: text/plain\r\n\r\nfirst\r\n..second\r\nlast\r\n.\r\n")
        .await
        .unwrap();
    assert_code(&read_reply(&mut stream).await, 250);

    let messages = sender.messages();
    let body = text_body(&messages[0].parsed.body);
    assert!(body.contains(".second"), "leading dot should be unstuffed: {body:?}");
    assert!(!body.contains("..second"), "stuffing dot should be removed: {body:?}");

    handle.abort();
}

#[tokio::test]
async fn smtp_rejects_bad_command_ordering() {
    let (addr, sender, handle) = start_server(1024 * 1024).await;
    let mut stream = connect(addr).await;

    command(&mut stream, "EHLO client.example\r\n", 250).await;
    command(&mut stream, "RCPT TO:<a@example.com>\r\n", 503).await;
    command(&mut stream, "DATA\r\n", 503).await;

    assert_eq!(sender.len(), 0);
    handle.abort();
}

#[tokio::test]
async fn smtp_rejects_oversized_message() {
    let (addr, sender, handle) = start_server(64).await;
    let mut stream = connect(addr).await;

    command(&mut stream, "EHLO client.example\r\n", 250).await;
    command(&mut stream, "MAIL FROM:<sender@example.com>\r\n", 250).await;
    command(&mut stream, "RCPT TO:<a@example.com>\r\n", 250).await;
    command(&mut stream, "DATA\r\n", 354).await;
    stream
        .write_all(
            b"Subject: Oversized\r\nContent-Type: text/plain\r\n\r\n\
this message body is definitely much larger than the sixty four byte limit configured here\r\n.\r\n",
        )
        .await
        .unwrap();
    assert_code(&read_reply(&mut stream).await, 552);

    assert_eq!(sender.len(), 0);
    handle.abort();
}

#[tokio::test]
async fn smtp_accepts_partial_commands_across_tcp_packets() {
    let (addr, sender, handle) = start_server(1024 * 1024).await;
    let mut stream = connect(addr).await;

    stream.write_all(b"EH").await.unwrap();
    stream.write_all(b"LO client.example\r\n").await.unwrap();
    assert_code(&read_reply(&mut stream).await, 250);

    stream.write_all(b"MAIL FROM:<sender").await.unwrap();
    stream.write_all(b"@example.com>\r\n").await.unwrap();
    assert_code(&read_reply(&mut stream).await, 250);

    command(&mut stream, "RCPT TO:<a@example.com>\r\n", 250).await;
    command(&mut stream, "DATA\r\n", 354).await;
    stream
        .write_all(b"Subject: Split\r\nContent-Type: text/plain\r\n\r\npar")
        .await
        .unwrap();
    stream.write_all(b"tial\r\n").await.unwrap();
    stream.write_all(b".").await.unwrap();
    stream.write_all(b"\r\n").await.unwrap();
    assert_code(&read_reply(&mut stream).await, 250);

    let messages = sender.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].parsed.subject, "Split");
    assert!(text_body(&messages[0].parsed.body).contains("partial"));

    handle.abort();
}

#[tokio::test]
async fn smtp_rejects_recipient_over_directmail_limit() {
    let (addr, sender, handle) = start_server(1024 * 1024).await;
    let mut stream = connect(addr).await;

    command(&mut stream, "EHLO client.example\r\n", 250).await;
    command(&mut stream, "MAIL FROM:<sender@example.com>\r\n", 250).await;
    for i in 0..100 {
        command(&mut stream, &format!("RCPT TO:<user{i}@example.com>\r\n"), 250).await;
    }
    command(&mut stream, "RCPT TO:<overflow@example.com>\r\n", 452).await;

    assert_eq!(sender.len(), 0);
    handle.abort();
}

#[tokio::test]
async fn smtp_rejects_subject_over_100_chars() {
    let (addr, sender, handle) = start_server(1024 * 1024).await;
    let mut stream = connect(addr).await;
    let subject = "x".repeat(101);

    command(&mut stream, "EHLO client.example\r\n", 250).await;
    command(&mut stream, "MAIL FROM:<sender@example.com>\r\n", 250).await;
    command(&mut stream, "RCPT TO:<a@example.com>\r\n", 250).await;
    command(&mut stream, "DATA\r\n", 354).await;
    stream
        .write_all(
            format!("Subject: {subject}\r\nContent-Type: text/plain\r\n\r\nbody\r\n.\r\n").as_bytes(),
        )
        .await
        .unwrap();
    assert_code(&read_reply(&mut stream).await, 554);

    assert_eq!(sender.len(), 0);
    handle.abort();
}

#[tokio::test]
async fn smtp_enforces_max_connections() {
    let sender = Arc::new(RecordingSender::new());
    let mut config = GatewayConfig::for_test("127.0.0.1:0".parse().unwrap());
    config.max_connections = 1;
    let (addr, handle) = start_configured_server(config, sender).await;

    // The first connection takes the only permit and is greeted.
    let mut first = connect(addr).await;

    // The OS backlog may complete the TCP handshake for the second client, but the gateway never
    // accepts it while the permit is held — so no 220 greeting arrives.
    let mut second = timeout(Duration::from_secs(1), TcpStream::connect(addr))
        .await
        .expect("second TCP connect timed out")
        .expect("second TCP connect failed");
    let second_greeting = timeout(Duration::from_millis(150), read_reply(&mut second)).await;
    assert!(
        second_greeting.is_err(),
        "second connection must not be greeted while the only permit is held"
    );

    // Releasing the first connection frees the permit; the second is then served.
    command(&mut first, "QUIT\r\n", 221).await;
    let reply = timeout(Duration::from_secs(1), read_reply(&mut second))
        .await
        .expect("second connection was not served after the first closed");
    assert_code(&reply, 220);

    handle.abort();
}

/// Sender that records its call count then sleeps far past any test timeout, to exercise the
/// delivery-timeout path without contacting a real provider.
#[derive(Clone)]
struct SleepingSender {
    calls: Arc<AtomicUsize>,
    sleep_for: Duration,
}

#[async_trait]
impl MailSender for SleepingSender {
    async fn send(&self, _msg: OutgoingMail) -> Result<SendOutcome, SendError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        sleep(self.sleep_for).await;
        Ok(SendOutcome::default())
    }
}

#[tokio::test]
async fn smtp_returns_451_when_sender_times_out() {
    let calls = Arc::new(AtomicUsize::new(0));
    let sender = Arc::new(SleepingSender {
        calls: Arc::clone(&calls),
        sleep_for: Duration::from_secs(5),
    });
    let mut config = GatewayConfig::for_test("127.0.0.1:0".parse().unwrap());
    config.send_timeout = Duration::from_millis(40);
    let (addr, handle) = start_configured_server(config, sender).await;
    let mut stream = connect(addr).await;

    command(&mut stream, "EHLO client.example\r\n", 250).await;
    command(&mut stream, "MAIL FROM:<sender@example.com>\r\n", 250).await;
    command(&mut stream, "RCPT TO:<a@example.com>\r\n", 250).await;
    command(&mut stream, "DATA\r\n", 354).await;
    stream
        .write_all(b"Subject: Slow\r\nContent-Type: text/plain\r\n\r\nbody\r\n.\r\n")
        .await
        .unwrap();
    assert_code(&read_reply(&mut stream).await, 451);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // The transaction was reset on timeout, so a fresh MAIL FROM is accepted (250, not 503).
    command(&mut stream, "MAIL FROM:<sender@example.com>\r\n", 250).await;

    handle.abort();
}

#[tokio::test]
async fn smtp_mime_multipart_alternative_prefers_html() {
    let sender = submit_raw_message(
        "Subject: Alternative\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"alt\"\r\n\
\r\n\
--alt\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
plain body\r\n\
--alt\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>html body</p>\r\n\
--alt--",
        250,
    )
    .await;

    let messages = sender.messages();
    assert_eq!(messages.len(), 1);
    assert!(html_body(&messages[0].parsed.body).contains("html body"));
}

#[tokio::test]
async fn smtp_mime_related_inline_image_is_rejected() {
    let sender = submit_raw_message(
        "Subject: Related\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"rel\"\r\n\
\r\n\
--rel\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p><img src=\"cid:image1\"></p>\r\n\
--rel\r\n\
Content-Type: image/png\r\n\
Content-Transfer-Encoding: base64\r\n\
Content-ID: <image1>\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--rel--",
        554,
    )
    .await;

    assert_eq!(sender.len(), 0);
}

#[tokio::test]
async fn smtp_mime_mixed_pdf_attachment_is_rejected() {
    let sender = submit_raw_message(
        "Subject: Attachment\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"mix\"\r\n\
\r\n\
--mix\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
body\r\n\
--mix\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment; filename=\"file.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQK\r\n\
--mix--",
        554,
    )
    .await;

    assert_eq!(sender.len(), 0);
}

#[tokio::test]
async fn smtp_mime_mixed_extra_text_part_is_rejected() {
    let sender = submit_raw_message(
        "Subject: Extra text\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"mix\"\r\n\
\r\n\
--mix\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
first body\r\n\
--mix\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
second body\r\n\
--mix--",
        554,
    )
    .await;

    assert_eq!(sender.len(), 0);
}

#[tokio::test]
async fn smtp_mime_mixed_text_and_html_is_rejected() {
    // text + HTML is only an alternative under multipart/alternative; in a mixed container the two
    // are distinct content, so accepting it would silently drop the text part.
    let sender = submit_raw_message(
        "Subject: Mixed bodies\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"mix\"\r\n\
\r\n\
--mix\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
plain body\r\n\
--mix\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>html body</p>\r\n\
--mix--",
        554,
    )
    .await;

    assert_eq!(sender.len(), 0);
}

#[tokio::test]
async fn smtp_mime_standalone_calendar_is_rejected() {
    let sender = submit_raw_message(
        "Subject: Calendar\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/calendar; charset=utf-8\r\n\
\r\n\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
END:VCALENDAR",
        554,
    )
    .await;

    assert_eq!(sender.len(), 0);
}

#[tokio::test]
async fn smtp_mime_nested_message_rfc822_is_rejected() {
    let sender = submit_raw_message(
        "Subject: Nested\r\n\
MIME-Version: 1.0\r\n\
Content-Type: message/rfc822\r\n\
\r\n\
From: nested@example.com\r\n\
Subject: Inner\r\n\
Content-Type: text/plain\r\n\
\r\n\
inner body",
        554,
    )
    .await;

    assert_eq!(sender.len(), 0);
}
