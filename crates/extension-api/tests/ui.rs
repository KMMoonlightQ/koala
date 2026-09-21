use koala_extension_api::Response;

#[test]
fn accepts_interactive_ui_response() {
    let response = serde_json::from_str::<Response>(
        r#"{"ui":[{"type":"set_widget","id":"docs","placement":"above_editor","blocks":[{"type":"select","id":"doc","label":"Choose","options":[{"id":"a","label":"A"}]}]}]}"#,
    );
    assert!(response.is_ok(), "v2 UI response rejected: {response:?}");
}

#[test]
fn rejects_ambiguous_and_unbounded_ui() {
    use koala_extension_api::validate_ui;
    for blocks in [
        r#"[{"type":"button","id":"same","label":"A"},{"type":"input","id":"same","label":"B"}]"#
            .to_owned(),
        r#"[{"type":"select","id":"s","label":"S","options":[]}]"#.to_owned(),
        format!(r#"[{{"type":"text","text":"{}"}}]"#, "x".repeat(16385)),
    ] {
        let value = format!(
            r#"{{"ui":[{{"type":"set_widget","id":"w","placement":"above_editor","blocks":{blocks}}}]}}"#
        );
        let response: Response = serde_json::from_str(&value).unwrap();
        assert!(validate_ui(&response.ui).is_err());
    }
    assert!(
        serde_json::from_str::<Response>(
            r#"{"ui":[{"type":"notify","level":"info","text":"hi","extension":"spoof"}]}"#
        )
        .is_err()
    );
}
