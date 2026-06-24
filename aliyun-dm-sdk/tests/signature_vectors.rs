use aliyun_dm_sdk::signer::{
    canonical_query, percent_encode, signature, signed_query, string_to_sign,
};

#[test]
fn describe_regions_official_get_vector() {
    // Canonical Aliyun RPC sample (AccessKeyId=testid, AccessKeySecret=testsecret). The
    // StringToSign below is the documented value, so a matching signature proves the encoder.
    let params = vec![
        ("Timestamp".to_owned(), "2016-02-23T12:46:24Z".to_owned()),
        ("Format".to_owned(), "XML".to_owned()),
        ("AccessKeyId".to_owned(), "testid".to_owned()),
        ("Action".to_owned(), "DescribeRegions".to_owned()),
        ("SignatureMethod".to_owned(), "HMAC-SHA1".to_owned()),
        (
            "SignatureNonce".to_owned(),
            "3ee8c1b8-83d3-44af-a94f-4e0ad82fd6cf".to_owned(),
        ),
        ("Version".to_owned(), "2014-05-26".to_owned()),
        ("SignatureVersion".to_owned(), "1.0".to_owned()),
    ];

    let canonical = canonical_query(&params);
    assert_eq!(
        canonical,
        "AccessKeyId=testid&Action=DescribeRegions&Format=XML&SignatureMethod=HMAC-SHA1&SignatureNonce=3ee8c1b8-83d3-44af-a94f-4e0ad82fd6cf&SignatureVersion=1.0&Timestamp=2016-02-23T12%3A46%3A24Z&Version=2014-05-26"
    );
    assert_eq!(
        string_to_sign("GET", &canonical),
        "GET&%2F&AccessKeyId%3Dtestid%26Action%3DDescribeRegions%26Format%3DXML%26SignatureMethod%3DHMAC-SHA1%26SignatureNonce%3D3ee8c1b8-83d3-44af-a94f-4e0ad82fd6cf%26SignatureVersion%3D1.0%26Timestamp%3D2016-02-23T12%253A46%253A24Z%26Version%3D2014-05-26"
    );
    assert_eq!(
        signature("GET", &params, "testsecret"),
        "OLeaidS1JvxuMvnyHOwuJ+uX5qY="
    );
}

#[test]
fn directmail_single_send_mail_post_vector() {
    // Exercises the POST path and special characters. The StringToSign is produced by the
    // encoder itself; the signature is HMAC-SHA1 over it, which is deterministic and standard.
    let params = vec![
        ("AccessKeyId".to_owned(), "testid".to_owned()),
        ("AccountName".to_owned(), "<a%b'>".to_owned()),
        ("Action".to_owned(), "SingleSendMail".to_owned()),
        ("AddressType".to_owned(), "1".to_owned()),
        ("Format".to_owned(), "XML".to_owned()),
        ("HtmlBody".to_owned(), "4".to_owned()),
        ("RegionId".to_owned(), "cn-hangzhou".to_owned()),
        ("ReplyToAddress".to_owned(), "true".to_owned()),
        ("SignatureMethod".to_owned(), "HMAC-SHA1".to_owned()),
        (
            "SignatureNonce".to_owned(),
            "c1b2c332-4cfb-4a0f-b8cc-ebe622aa0a5c".to_owned(),
        ),
        ("SignatureVersion".to_owned(), "1.0".to_owned()),
        ("Subject".to_owned(), "3".to_owned()),
        ("TagName".to_owned(), "2".to_owned()),
        ("Timestamp".to_owned(), "2016-10-20T06:27:56Z".to_owned()),
        ("ToAddress".to_owned(), "1@test.com".to_owned()),
        ("Version".to_owned(), "2015-11-23".to_owned()),
    ];

    let canonical = canonical_query(&params);
    assert_eq!(
        string_to_sign("POST", &canonical),
        "POST&%2F&AccessKeyId%3Dtestid%26AccountName%3D%253Ca%2525b%2527%253E%26Action%3DSingleSendMail%26AddressType%3D1%26Format%3DXML%26HtmlBody%3D4%26RegionId%3Dcn-hangzhou%26ReplyToAddress%3Dtrue%26SignatureMethod%3DHMAC-SHA1%26SignatureNonce%3Dc1b2c332-4cfb-4a0f-b8cc-ebe622aa0a5c%26SignatureVersion%3D1.0%26Subject%3D3%26TagName%3D2%26Timestamp%3D2016-10-20T06%253A27%253A56Z%26ToAddress%3D1%2540test.com%26Version%3D2015-11-23"
    );
    assert_eq!(
        signature("POST", &params, "testsecret"),
        "llJfXJjBW3OacrVgxxsITgYaYm0="
    );
}

#[test]
fn percent_encoding_boundaries() {
    assert_eq!(percent_encode("*"), "%2A");
    assert_eq!(percent_encode(" "), "%20");
    assert_eq!(percent_encode("+"), "%2B");
    assert_eq!(percent_encode("~"), "~");
    assert_eq!(percent_encode("中文"), "%E4%B8%AD%E6%96%87");
    assert_eq!(
        percent_encode("* + ~ 中文 空格"),
        "%2A%20%2B%20~%20%E4%B8%AD%E6%96%87%20%E7%A9%BA%E6%A0%BC"
    );
    assert_eq!(percent_encode("<a%b'>"), "%3Ca%25b%27%3E");
    assert_eq!(percent_encode("1@test.com"), "1%40test.com");
}

#[test]
fn signed_query_encodes_signature_once() {
    let params = vec![
        ("Action".to_owned(), "DescribeRegions".to_owned()),
        ("AccessKeyId".to_owned(), "testid".to_owned()),
        ("Format".to_owned(), "XML".to_owned()),
        ("Signature".to_owned(), "this value must be ignored".to_owned()),
        ("SignatureMethod".to_owned(), "HMAC-SHA1".to_owned()),
        (
            "SignatureNonce".to_owned(),
            "3ee8c1b8-83d3-44af-a94f-4e0ad82fd6cf".to_owned(),
        ),
        ("SignatureVersion".to_owned(), "1.0".to_owned()),
        ("Timestamp".to_owned(), "2016-02-23T12:46:24Z".to_owned()),
        ("Version".to_owned(), "2014-05-26".to_owned()),
    ];

    let query = signed_query("GET", &params, "testsecret");

    assert!(query.ends_with("Signature=OLeaidS1JvxuMvnyHOwuJ%2BuX5qY%3D"));
    assert!(!query.contains("this%20value%20must%20be%20ignored"));
    assert!(!query.contains("%252B"));
}
