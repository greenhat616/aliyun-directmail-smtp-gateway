use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

pub fn percent_encode(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len());

    for &byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push('%');
                encoded.push(hex_upper(byte >> 4));
                encoded.push(hex_upper(byte & 0x0f));
            }
        }
    }

    encoded
}

pub fn canonical_query(params: &[(String, String)]) -> String {
    let mut sorted: BTreeMap<&str, &str> = BTreeMap::new();

    for (key, value) in params {
        if key != "Signature" {
            sorted.insert(key.as_str(), value.as_str());
        }
    }

    sorted
        .into_iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

pub fn string_to_sign(method: &str, canonical_query: &str) -> String {
    format!(
        "{}&%2F&{}",
        method.to_ascii_uppercase(),
        percent_encode(canonical_query)
    )
}

pub fn signature(method: &str, params: &[(String, String)], access_key_secret: &str) -> String {
    sign_string(
        &string_to_sign(method, &canonical_query(params)),
        access_key_secret,
    )
}

pub fn sign_string(string_to_sign: &str, access_key_secret: &str) -> String {
    let signing_key = format!("{access_key_secret}&");
    let mut mac =
        HmacSha1::new_from_slice(signing_key.as_bytes()).expect("HMAC-SHA1 accepts any key length");

    mac.update(string_to_sign.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}

pub fn signed_query(
    method: &str,
    params: &[(String, String)],
    access_key_secret: &str,
) -> String {
    let canonical = canonical_query(params);
    let sig = sign_string(&string_to_sign(method, &canonical), access_key_secret);
    let signature_pair = format!("Signature={}", percent_encode(&sig));

    if canonical.is_empty() {
        signature_pair
    } else {
        format!("{canonical}&{signature_pair}")
    }
}

fn hex_upper(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'A' + (nibble - 10)) as char,
        _ => unreachable!("nibble is always four bits"),
    }
}
