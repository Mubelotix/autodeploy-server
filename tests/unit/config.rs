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
fn configuration_requires_every_setting() {
    let error = match parse_config(
        "[command test]\napi_key = 00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa\ngroup = users\nexecutable = /bin/true\narguments = []\n",
    ) {
        Ok(_) => panic!("configuration unexpectedly parsed"),
        Err(error) => error,
    };
    assert!(error.contains("missing user"));
}
