use super::*;

#[test]
fn parses_empty_and_quoted_arguments() {
    assert_eq!(parse_arguments("[]").unwrap(), Vec::<String>::new());
    assert_eq!(
        parse_arguments(r#"["one", "two words"]"#).unwrap(),
        ["one", "two words"]
    );
}

#[test]
fn rejects_invalid_arguments() {
    assert!(parse_arguments("[not-a-string]").is_err());
    assert!(parse_arguments("[\"one\",]").is_err());
    assert!(parse_arguments("one, two").is_err());
}

#[test]
fn parses_only_valid_api_keys() {
    assert_eq!(parse_api_key(&"aB".repeat(32)).unwrap(), [0xab; 32]);
    assert!(parse_api_key("a").is_err());
    assert!(parse_api_key(&"zz".repeat(32)).is_err());
}

#[test]
fn parses_only_positive_timeouts() {
    assert_eq!(parse_timeout("1").unwrap(), 1);
    assert!(parse_timeout("0").is_err());
    assert!(parse_timeout("one").is_err());
}

#[test]
fn parses_only_valid_ports() {
    assert_eq!(parse_port("8080").unwrap(), 8080);
    assert!(parse_port("0").is_err());
    assert!(parse_port("65536").is_err());
}

#[test]
fn configuration_requires_every_setting() {
    let error = match parse_config(
        "port = 8080\n[command test]\napi_key = 00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa\ngroup = users\nexecutable = /bin/true\narguments = []\ntimeout = 30\n",
    ) {
        Ok(_) => panic!("configuration unexpectedly parsed"),
        Err(error) => error,
    };
    assert!(error.contains("missing user"));
}
