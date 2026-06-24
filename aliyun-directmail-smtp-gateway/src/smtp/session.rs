use std::sync::Arc;

use smtp_proto::request::receiver::{DataReceiver, DummyDataReceiver, RequestReceiver};
use smtp_proto::{Error as ParseError, Request};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::config::GatewayConfig;
use crate::error::{send_error_reply, SessionError};
use crate::mail::{OutgoingMail, ParsedMail};
use crate::sender::MailSender;
use crate::smtp::reply;

/// Size of each TCP read. Also bounds how far a message may exceed `max_message_bytes` before the
/// session switches to discard mode (by at most one buffer's worth).
const READ_CHUNK: usize = 8 * 1024;

/// What the read loop should do after handling one command.
#[derive(PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
}

/// Which sub-protocol the connection is currently reading.
enum Mode {
    /// Reading SMTP commands.
    Command,
    /// Accumulating `DATA` content into `data_buf`.
    Data(DataReceiver),
    /// Draining an oversized `DATA` stream to its terminator without buffering it.
    Discard(DummyDataReceiver),
}

/// Cheap, borrow-free discriminant of [`Mode`], used to pick a read-loop branch without holding a
/// borrow of `self.mode` across the handler calls.
enum ModeTag {
    Command,
    Data,
    Discard,
}

/// A single SMTP connection: owns the stream, the parser receivers, and the in-flight transaction.
///
/// Holds `config`/`sender` behind `Arc` (not borrows) so the whole session can be moved into a
/// spawned `'static` task per connection.
pub struct Session<S> {
    stream: S,
    config: Arc<GatewayConfig>,
    sender: Arc<dyn MailSender>,
    request_rx: RequestReceiver,
    mode: Mode,
    greeted: bool,
    envelope_from: Option<String>,
    recipients: Vec<String>,
    data_buf: Vec<u8>,
}

impl<S> Session<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    pub fn new(stream: S, config: Arc<GatewayConfig>, sender: Arc<dyn MailSender>) -> Self {
        Self {
            stream,
            config,
            sender,
            request_rx: RequestReceiver::default(),
            mode: Mode::Command,
            greeted: false,
            envelope_from: None,
            recipients: Vec::new(),
            data_buf: Vec::new(),
        }
    }

    /// Drives the connection from the `220` greeting until `QUIT`, EOF, or a fatal I/O error.
    pub async fn run(mut self) -> Result<(), SessionError> {
        let greeting = format!(
            "220 {} ESMTP aliyun-directmail-smtp-gateway\r\n",
            self.config.hostname
        );
        self.write(&greeting).await?;

        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            let read = timeout(self.config.read_timeout, self.stream.read(&mut buf)).await;
            let n = match read {
                Ok(Ok(0)) => return Ok(()), // client closed the connection
                Ok(Ok(n)) => n,
                Ok(Err(err)) => return Err(SessionError::Io(err)),
                Err(_) => {
                    self.write(&reply::enhanced(421, "4.4.2", "Idle timeout, closing connection"))
                        .await?;
                    return Ok(());
                }
            };

            let mut iter = buf[..n].iter();
            while !iter.as_slice().is_empty() {
                // Each branch scopes its borrow of `self` tightly: the parser receivers are
                // borrowed only for the `ingest` call, then released before any `&mut self`
                // handler runs.
                match self.mode_tag() {
                    ModeTag::Command => match self.request_rx.ingest(&mut iter) {
                        Ok(request) => {
                            let request = request.into_owned();
                            if self.dispatch(request).await? == Flow::Quit {
                                return Ok(());
                            }
                        }
                        Err(ParseError::NeedsMoreData { .. }) => break,
                        Err(err) => self.write_parse_error(&err).await?,
                    },
                    ModeTag::Data => {
                        let complete = match &mut self.mode {
                            Mode::Data(receiver) => receiver.ingest(&mut iter, &mut self.data_buf),
                            _ => unreachable!("mode tag is Data"),
                        };
                        if !complete {
                            if self.data_buf.len() > self.config.max_message_bytes {
                                self.enter_discard();
                            }
                            // `ingest` consumed the whole chunk; fall through to read more.
                        } else if self.data_buf.len() > self.config.max_message_bytes {
                            self.reject_oversized().await?;
                        } else {
                            self.finalize_data().await?;
                        }
                    }
                    ModeTag::Discard => {
                        let complete = match &mut self.mode {
                            Mode::Discard(receiver) => receiver.ingest(&mut iter),
                            _ => unreachable!("mode tag is Discard"),
                        };
                        if complete {
                            self.reject_oversized().await?;
                        }
                    }
                }
            }
        }
    }

    fn mode_tag(&self) -> ModeTag {
        match self.mode {
            Mode::Command => ModeTag::Command,
            Mode::Data(_) => ModeTag::Data,
            Mode::Discard(_) => ModeTag::Discard,
        }
    }

    async fn dispatch(&mut self, request: Request<String>) -> Result<Flow, SessionError> {
        match request {
            Request::Ehlo { .. } => {
                self.greeted = true;
                self.reset_transaction();
                let response = reply::ehlo(&self.config.hostname, self.config.max_message_bytes);
                self.write(&response).await?;
            }
            Request::Helo { .. } => {
                self.greeted = true;
                self.reset_transaction();
                self.write(&reply::line(250, &self.config.hostname.clone())).await?;
            }
            Request::Mail { from } => self.handle_mail(from.address, from.size).await?,
            Request::Rcpt { to } => self.handle_rcpt(to.address).await?,
            Request::Data => self.handle_data().await?,
            Request::Rset => {
                self.reset_transaction();
                self.write(&reply::enhanced(250, "2.0.0", "Ok")).await?;
            }
            Request::Noop { .. } => {
                self.write(&reply::enhanced(250, "2.0.0", "Ok")).await?;
            }
            Request::Quit => {
                self.write(&reply::enhanced(221, "2.0.0", "Bye")).await?;
                return Ok(Flow::Quit);
            }
            // BDAT/CHUNKING, STARTTLS, AUTH and the rarely-used verbs are out of scope this stage.
            _ => {
                self.write(&reply::enhanced(502, "5.5.1", "Command not implemented"))
                    .await?;
            }
        }
        Ok(Flow::Continue)
    }

    async fn handle_mail(&mut self, address: String, size: usize) -> Result<(), SessionError> {
        if !self.greeted {
            return self
                .write(&reply::enhanced(503, "5.5.1", "Send EHLO/HELO first"))
                .await;
        }
        if self.envelope_from.is_some() {
            return self
                .write(&reply::enhanced(503, "5.5.1", "Sender already specified"))
                .await;
        }
        if size > 0 && size > self.config.max_message_bytes {
            return self
                .write(&reply::enhanced(552, "5.3.4", "Message size exceeds maximum"))
                .await;
        }
        self.envelope_from = Some(address);
        self.recipients.clear();
        self.write(&reply::enhanced(250, "2.1.0", "Sender ok")).await
    }

    async fn handle_rcpt(&mut self, address: String) -> Result<(), SessionError> {
        if self.envelope_from.is_none() {
            return self
                .write(&reply::enhanced(503, "5.5.1", "Need MAIL FROM first"))
                .await;
        }
        if self.recipients.len() >= self.config.max_recipients {
            return self
                .write(&reply::enhanced(452, "4.5.3", "Too many recipients"))
                .await;
        }
        self.recipients.push(address);
        self.write(&reply::enhanced(250, "2.1.5", "Recipient ok")).await
    }

    async fn handle_data(&mut self) -> Result<(), SessionError> {
        if self.envelope_from.is_none() {
            return self
                .write(&reply::enhanced(503, "5.5.1", "Need MAIL FROM first"))
                .await;
        }
        if self.recipients.is_empty() {
            return self
                .write(&reply::enhanced(503, "5.5.1", "Need RCPT TO first"))
                .await;
        }
        self.data_buf.clear();
        self.mode = Mode::Data(DataReceiver::new());
        self.write(&reply::line(354, "Start mail input; end with <CRLF>.<CRLF>"))
            .await
    }

    /// Switches an in-progress `DATA` stream to discard mode, freeing the buffered bytes while
    /// preserving the dot/CRLF parser state so the terminator is still detected correctly.
    fn enter_discard(&mut self) {
        if let Mode::Data(receiver) = &self.mode {
            let dummy = DummyDataReceiver::new_data(receiver);
            self.data_buf = Vec::new();
            self.mode = Mode::Discard(dummy);
        }
    }

    async fn reject_oversized(&mut self) -> Result<(), SessionError> {
        warn!("rejected oversized message exceeding {} bytes", self.config.max_message_bytes);
        self.reset_transaction();
        self.write(&reply::enhanced(552, "5.3.4", "Message size exceeds maximum"))
            .await
    }

    /// Parses the accumulated message, delivers it through the sender, and replies with the outcome.
    async fn finalize_data(&mut self) -> Result<(), SessionError> {
        let raw = std::mem::take(&mut self.data_buf);
        let envelope_from = self.envelope_from.clone().unwrap_or_default();
        let recipients = std::mem::take(&mut self.recipients);

        let parsed = match ParsedMail::from_raw(&raw) {
            Ok(parsed) => parsed,
            Err(error) => {
                debug!(%error, "rejected message content");
                self.reset_transaction();
                return self
                    .write(&reply::enhanced(554, "5.6.0", &error.to_string()))
                    .await;
            }
        };

        let mail = OutgoingMail { envelope_from, recipients, raw_message: raw, parsed };
        // Cancelling `send` on timeout discards the in-flight request, but the provider may have
        // already accepted the message. We reply 451 so the client retries — accepting at-least-once
        // semantics (potential duplicate delivery) as the stage-one tradeoff.
        let result = timeout(self.config.send_timeout, self.sender.send(mail)).await;
        self.reset_transaction();

        match result {
            Ok(Ok(outcome)) => {
                info!(
                    provider_id = ?outcome.provider_id,
                    request_id = ?outcome.request_id,
                    "message accepted"
                );
                self.write(&reply::enhanced(250, "2.0.0", "Ok, message accepted"))
                    .await
            }
            Ok(Err(error)) => {
                warn!(%error, "delivery failed");
                let (code, esc, detail) = send_error_reply(&error);
                self.write(&reply::enhanced(code, esc, &detail)).await
            }
            Err(_) => {
                warn!(timeout = ?self.config.send_timeout, "delivery backend timed out");
                self.write(&reply::enhanced(451, "4.3.0", "Delivery backend timed out"))
                    .await
            }
        }
    }

    async fn write_parse_error(&mut self, error: &ParseError) -> Result<(), SessionError> {
        let (code, esc, message): (u16, &str, String) = match error {
            ParseError::UnknownCommand => {
                (500, "5.5.1", "Command not recognized".to_owned())
            }
            ParseError::InvalidSenderAddress => {
                (501, "5.1.7", "Bad sender address syntax".to_owned())
            }
            ParseError::InvalidRecipientAddress => {
                (501, "5.1.3", "Bad recipient address syntax".to_owned())
            }
            ParseError::SyntaxError { syntax } => {
                (501, "5.5.2", format!("Syntax error, expected: {syntax}"))
            }
            ParseError::InvalidParameter { param } => {
                (504, "5.5.4", format!("Unsupported parameter: {param}"))
            }
            ParseError::UnsupportedParameter { param } => {
                (504, "5.5.4", format!("Unsupported parameter: {param}"))
            }
            ParseError::ResponseTooLong => {
                (500, "5.5.2", "Line too long".to_owned())
            }
            // NeedsMoreData is handled by the caller; InvalidResponse is server-side only.
            ParseError::NeedsMoreData { .. } | ParseError::InvalidResponse { .. } => {
                (500, "5.5.1", "Command not recognized".to_owned())
            }
        };
        self.write(&reply::enhanced(code, esc, &message)).await
    }

    fn reset_transaction(&mut self) {
        self.envelope_from = None;
        self.recipients.clear();
        self.data_buf.clear();
        self.mode = Mode::Command;
    }

    async fn write(&mut self, data: &str) -> Result<(), SessionError> {
        self.stream.write_all(data.as_bytes()).await?;
        self.stream.flush().await?;
        Ok(())
    }
}
