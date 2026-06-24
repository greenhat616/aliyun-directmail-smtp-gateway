//! SMTP reply formatting. We emit our own replies rather than using `smtp-proto`'s response
//! generator so that the advertised capabilities and enhanced status codes stay under our control.

/// Builds the multi-line `EHLO` greeting. Advertises only the extensions this stage supports:
/// `SIZE` (with the configured limit) and `8BITMIME`. AUTH/STARTTLS are deliberately omitted.
pub fn ehlo(hostname: &str, max_message_bytes: usize) -> String {
    format!(
        "250-{hostname}\r\n250-SIZE {max_message_bytes}\r\n250 8BITMIME\r\n"
    )
}

/// Formats a single-line reply: `<code> <message>\r\n`.
pub fn line(code: u16, message: &str) -> String {
    format!("{code} {message}\r\n")
}

/// Formats a single-line reply with an RFC 3463 enhanced status code:
/// `<code> <esc> <message>\r\n`.
pub fn enhanced(code: u16, esc: &str, message: &str) -> String {
    format!("{code} {esc} {message}\r\n")
}
