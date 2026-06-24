use mail_parser::{MessageParser, MimeHeaders, PartType};

use aliyun_dm_sdk::{MailBody, SingleSendMailRequest};

/// Maximum DirectMail subject length, in scalar characters. Subjects beyond this are rejected
/// rather than truncated, to avoid silently changing the message semantics.
const MAX_SUBJECT_CHARS: usize = 100;

/// Maximum `FromAlias` length DirectMail accepts. A display name at or above this is dropped
/// (the mail is still sent without an alias) rather than truncated.
const MAX_FROM_ALIAS_CHARS: usize = 15;

const DEFAULT_SUBJECT: &str = "(no subject)";

/// A fully accepted message: the SMTP envelope plus the parsed, send-ready content.
#[derive(Debug, Clone)]
pub struct OutgoingMail {
    /// Envelope sender from `MAIL FROM`. Retained for logging only; never used as a trusted
    /// identity — DirectMail's account is the verified sender.
    pub envelope_from: String,
    /// Envelope recipients from the `RCPT TO` list; these become DirectMail's `ToAddress`.
    pub recipients: Vec<String>,
    pub raw_message: Vec<u8>,
    pub parsed: ParsedMail,
}

#[derive(Debug, Clone)]
pub struct ParsedMail {
    pub subject: String,
    pub from_alias: Option<String>,
    pub body: ParsedBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedBody {
    Html(String),
    Text(String),
}

/// Why a syntactically valid message cannot be turned into a DirectMail request.
///
/// These are content/policy rejections surfaced to the client as permanent failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Unparseable,
    SubjectTooLong { chars: usize },
    AttachmentsUnsupportedYet { count: usize },
    UnsupportedBodyStructure { detail: &'static str },
    EmptyBody,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unparseable => write!(f, "message could not be parsed as RFC 822"),
            Self::SubjectTooLong { chars } => {
                write!(f, "subject is {chars} characters; limit is {MAX_SUBJECT_CHARS}")
            }
            Self::AttachmentsUnsupportedYet { count } => {
                write!(f, "attachments are not supported ({count} present)")
            }
            Self::UnsupportedBodyStructure { detail } => f.write_str(detail),
            Self::EmptyBody => write!(f, "message has no text or HTML body"),
        }
    }
}

impl std::error::Error for ParseError {}

impl ParsedMail {
    /// Parses raw RFC 822 bytes into a send-ready [`ParsedMail`], applying the stage-one policy:
    /// reject attachments, reject empty bodies, reject over-long subjects, prefer HTML over text,
    /// and only keep a `From` display name short enough for DirectMail's `FromAlias`.
    pub fn from_raw(raw: &[u8]) -> Result<Self, ParseError> {
        let message = MessageParser::default()
            .parse(raw)
            .ok_or(ParseError::Unparseable)?;

        if message.attachment_count() > 0 {
            return Err(ParseError::AttachmentsUnsupportedYet {
                count: message.attachment_count(),
            });
        }

        // Count only real HTML/text parts (not the text->HTML alternative `mail-parser` synthesizes).
        // We can map exactly one body to DirectMail: a single HTML part wins, else a single text
        // part. Anything richer would silently drop content, since `from_raw` later sends only the
        // chosen body. Reject those structures: multiple HTML or multiple text parts, and a
        // text+HTML pair unless they are genuine alternatives (`multipart/alternative`) where
        // dropping the text representation is the intended semantics.
        let real_html = (0..message.html_body_count() as u32)
            .filter(|&index| matches!(message.html_part(index).map(|part| &part.body), Some(PartType::Html(_))))
            .count();
        let real_text = (0..message.text_body_count() as u32)
            .filter(|&index| matches!(message.text_part(index).map(|part| &part.body), Some(PartType::Text(_))))
            .count();
        if real_html > 1 {
            return Err(ParseError::UnsupportedBodyStructure {
                detail: "message has multiple HTML bodies",
            });
        }
        if real_text > 1 {
            return Err(ParseError::UnsupportedBodyStructure {
                detail: "message has multiple text bodies",
            });
        }
        if real_html > 0
            && real_text > 0
            && !message.root_part().is_content_type("multipart", "alternative")
        {
            return Err(ParseError::UnsupportedBodyStructure {
                detail: "message has multiple body representations outside multipart/alternative",
            });
        }

        let subject = match message.subject() {
            Some(subject) if !subject.is_empty() => subject.to_owned(),
            _ => DEFAULT_SUBJECT.to_owned(),
        };
        let subject_chars = subject.chars().count();
        if subject_chars > MAX_SUBJECT_CHARS {
            return Err(ParseError::SubjectTooLong { chars: subject_chars });
        }

        // `mail-parser` lists a single text/plain part as available for both the text AND html
        // bodies (it will synthesize HTML from text on demand). To honour "prefer a real HTML
        // part, else text/plain" we must inspect the actual part type, not the body counts.
        let has_real_html = matches!(
            message.html_part(0).map(|part| &part.body),
            Some(PartType::Html(_))
        );

        let body = if has_real_html {
            message
                .body_html(0)
                .map(|html| ParsedBody::Html(html.into_owned()))
        } else if message.text_body_count() > 0 {
            message
                .body_text(0)
                .map(|text| ParsedBody::Text(text.into_owned()))
        } else {
            None
        };

        let body = match body {
            Some(body) if !body.is_empty() => body,
            _ => return Err(ParseError::EmptyBody),
        };

        let from_alias = message
            .from()
            .and_then(|address| address.first())
            .and_then(|addr| addr.name())
            .map(str::trim)
            .filter(|name| !name.is_empty() && name.chars().count() < MAX_FROM_ALIAS_CHARS)
            .map(str::to_owned);

        Ok(Self { subject, from_alias, body })
    }
}

impl ParsedBody {
    fn is_empty(&self) -> bool {
        match self {
            Self::Html(body) | Self::Text(body) => body.is_empty(),
        }
    }

    fn into_mail_body(self) -> MailBody {
        match self {
            Self::Html(body) => MailBody::Html(body),
            Self::Text(body) => MailBody::Text(body),
        }
    }
}

impl OutgoingMail {
    /// Builds a DirectMail `SingleSendMail` request. `account_name` is the verified sender account
    /// from configuration — deliberately independent of the envelope/header `From`.
    pub fn to_directmail_request(
        &self,
        account_name: &str,
        address_type: u8,
        reply_to_address: bool,
    ) -> SingleSendMailRequest {
        let mut request = SingleSendMailRequest::new(
            account_name.to_owned(),
            self.recipients.clone(),
            self.parsed.subject.clone(),
            self.parsed.body.clone().into_mail_body(),
        )
        .with_reply_to_address(reply_to_address);
        request.address_type = address_type;

        if let Some(from_alias) = &self.parsed.from_alias {
            request = request.with_from_alias(from_alias.clone());
        }

        request
    }
}
