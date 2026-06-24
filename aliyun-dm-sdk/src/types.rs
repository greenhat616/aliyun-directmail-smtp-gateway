use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::DirectMailError;

/// DirectMail API generation. Both generations share an identical RPC transport and
/// HMAC-SHA1 signature; they differ only in the `Version` string, the supported Action
/// surface, and endpoint discovery. Defaults to the `2015-11-23` generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApiVersion {
    #[default]
    V20151123,
    V20170622,
}

impl ApiVersion {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V20151123 => "2015-11-23",
            Self::V20170622 => "2017-06-22",
        }
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailBody {
    Html(String),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleSendMailRequest {
    pub account_name: String,
    pub from_alias: Option<String>,
    pub address_type: u8,
    pub reply_to_address: bool,
    pub to_address: Vec<String>,
    pub subject: String,
    pub mail_body: MailBody,
}

impl SingleSendMailRequest {
    pub fn new(
        account_name: impl Into<String>,
        to_address: impl IntoRecipients,
        subject: impl Into<String>,
        mail_body: MailBody,
    ) -> Self {
        Self {
            account_name: account_name.into(),
            from_alias: None,
            address_type: 1,
            reply_to_address: false,
            to_address: to_address.into_recipients(),
            subject: subject.into(),
            mail_body,
        }
    }

    pub fn html(
        account_name: impl Into<String>,
        to_address: impl IntoRecipients,
        subject: impl Into<String>,
        html_body: impl Into<String>,
    ) -> Self {
        Self::new(account_name, to_address, subject, MailBody::Html(html_body.into()))
    }

    pub fn text(
        account_name: impl Into<String>,
        to_address: impl IntoRecipients,
        subject: impl Into<String>,
        text_body: impl Into<String>,
    ) -> Self {
        Self::new(account_name, to_address, subject, MailBody::Text(text_body.into()))
    }

    pub fn with_from_alias(mut self, from_alias: impl Into<String>) -> Self {
        self.from_alias = Some(from_alias.into());
        self
    }

    pub fn with_reply_to_address(mut self, reply_to_address: bool) -> Self {
        self.reply_to_address = reply_to_address;
        self
    }

    pub fn validate(&self) -> Result<(), DirectMailError> {
        if self.account_name.trim().is_empty() {
            return Err(DirectMailError::Validation(
                "AccountName must not be empty".to_owned(),
            ));
        }

        let recipients = self.normalized_recipients();
        if recipients.is_empty() {
            return Err(DirectMailError::Validation(
                "ToAddress must contain at least one recipient".to_owned(),
            ));
        }

        if recipients.len() > 100 {
            return Err(DirectMailError::Validation(
                "ToAddress supports at most 100 recipients".to_owned(),
            ));
        }

        for recipient in &recipients {
            if recipient.trim().is_empty() {
                return Err(DirectMailError::Validation(
                    "ToAddress must not contain empty recipients".to_owned(),
                ));
            }
        }

        if self.subject.chars().count() > 100 {
            return Err(DirectMailError::Validation(
                "Subject must be at most 100 characters".to_owned(),
            ));
        }

        let body_is_empty = match &self.mail_body {
            MailBody::Html(body) | MailBody::Text(body) => body.is_empty(),
        };
        if body_is_empty {
            return Err(DirectMailError::Validation(
                "HtmlBody or TextBody must not be empty".to_owned(),
            ));
        }

        if let Some(from_alias) = &self.from_alias
            && from_alias.chars().count() >= 15
        {
            return Err(DirectMailError::Validation(
                "FromAlias must be less than 15 characters".to_owned(),
            ));
        }

        Ok(())
    }

    pub fn to_query_params(&self, version: ApiVersion) -> Vec<(String, String)> {
        let mut params = vec![
            ("Action".to_owned(), "SingleSendMail".to_owned()),
            ("Version".to_owned(), version.as_str().to_owned()),
            ("Format".to_owned(), "JSON".to_owned()),
            ("AccountName".to_owned(), self.account_name.clone()),
            ("AddressType".to_owned(), self.address_type.to_string()),
            (
                "ReplyToAddress".to_owned(),
                self.reply_to_address.to_string(),
            ),
            ("ToAddress".to_owned(), self.normalized_recipients().join(",")),
            ("Subject".to_owned(), self.subject.clone()),
        ];

        if let Some(from_alias) = &self.from_alias {
            params.push(("FromAlias".to_owned(), from_alias.clone()));
        }

        match &self.mail_body {
            MailBody::Html(body) => params.push(("HtmlBody".to_owned(), body.clone())),
            MailBody::Text(body) => params.push(("TextBody".to_owned(), body.clone())),
        }

        params
    }

    fn normalized_recipients(&self) -> Vec<String> {
        self.to_address
            .iter()
            .flat_map(|recipient| recipient.split(','))
            .map(|recipient| recipient.trim().to_owned())
            .collect()
    }
}

pub trait IntoRecipients {
    fn into_recipients(self) -> Vec<String>;
}

impl IntoRecipients for String {
    fn into_recipients(self) -> Vec<String> {
        vec![self]
    }
}

impl IntoRecipients for &str {
    fn into_recipients(self) -> Vec<String> {
        vec![self.to_owned()]
    }
}

impl IntoRecipients for Vec<String> {
    fn into_recipients(self) -> Vec<String> {
        self
    }
}

impl<const N: usize> IntoRecipients for [&str; N] {
    fn into_recipients(self) -> Vec<String> {
        self.into_iter().map(str::to_owned).collect()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SingleSendMailResponse {
    #[serde(rename = "EnvId", alias = "env_id", alias = "envId")]
    pub env_id: String,
    #[serde(rename = "RequestId", alias = "request_id", alias = "requestId")]
    pub request_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn many_recipients(count: usize) -> String {
        (0..count)
            .map(|i| format!("u{i}@example.com"))
            .collect::<Vec<_>>()
            .join(",")
    }

    #[test]
    fn validate_counts_comma_separated_recipients() {
        let req = SingleSendMailRequest::text("a@example.com", many_recipients(101), "s", "body");
        let err = req.validate().unwrap_err();
        assert!(matches!(&err, DirectMailError::Validation(m) if m.contains("100")));
    }

    #[test]
    fn validate_accepts_exactly_one_hundred_recipients() {
        let req = SingleSendMailRequest::text("a@example.com", many_recipients(100), "s", "body");
        req.validate().unwrap();
    }

    #[test]
    fn validate_rejects_trailing_comma_recipient() {
        let req = SingleSendMailRequest::text("a@example.com", "u@example.com,", "s", "body");
        let err = req.validate().unwrap_err();
        assert!(matches!(&err, DirectMailError::Validation(m) if m.contains("empty recipients")));
    }
}
