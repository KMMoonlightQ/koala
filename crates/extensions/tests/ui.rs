use koala_extensions::*;

#[tokio::test]
async fn v2_process_receives_capabilities_and_mount() {
    let root = std::env::temp_dir().join(format!("koala-ui-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("extension.toml"),
        "api_version = 2\nname = 'ui-test'\nui = true\ncommand = ['python3', 'main.py']\n",
    )
    .unwrap();
    std::fs::write(root.join("main.py"), "import json,sys\nr=json.load(sys.stdin)\nassert r['api_version']==2\nassert r['capabilities']['ui']\nassert r['kind']=='ui_event'\nprint(json.dumps({'ui':[{'type':'set_status','id':'ready','text':r['event']['type']}]}))\n").unwrap();
    let loaded = load(
        &ExtensionsConfig {
            manifests: vec![root.join("extension.toml")],
            ..Default::default()
        },
        Vec::new(),
    );
    assert!(loaded.is_ok(), "v2 extension rejected: {:?}", loaded.err());
    let extension = loaded.unwrap().ui_extensions().pop().unwrap();
    let event = UiInputEvent {
        kind: UiEventType::Mount,
        event_id: "mount".into(),
        surface: None,
        surface_id: None,
        revision: 0,
        control_id: None,
        value: serde_json::Value::Null,
    };
    let response = ui::scope(
        ui::RequestContext {
            session_id: "session".into(),
            ui: true,
            ..Default::default()
        },
        extension.ui_event(&event),
    )
    .await
    .unwrap();
    assert!(matches!(&response.ui[0], UiCommand::SetStatus { text, .. } if text == "mount"));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn ui_requires_opt_in_and_invalid_response_publishes_nothing() {
    use std::sync::Arc;
    struct Invalid;
    impl Extension for Invalid {
        fn name(&self) -> &str {
            "invalid"
        }
        fn ui_enabled(&self) -> bool {
            true
        }
        fn hook<'a>(&'a self, _: Stage, _: &'a serde_json::Value) -> ExtensionFuture<'a> {
            Box::pin(async {
                Ok(Response {
                    arguments: Some(serde_json::json!([])),
                    ui: vec![UiCommand::Notify {
                        level: Level::Info,
                        text: "not published".into(),
                    }],
                    ..Default::default()
                })
            })
        }
    }
    let mut extensions = Extensions::default();
    extensions.register(Arc::new(Invalid)).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result = ui::scope(
        ui::RequestContext {
            ui: true,
            updates: Some(tx),
            ..Default::default()
        },
        extensions.hook(Stage::BeforeModel, serde_json::json!({})),
    )
    .await;
    assert!(result.is_err());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn v1_request_shape_stays_unchanged_and_v2_defaults_to_no_ui() {
    let root = std::env::temp_dir().join(format!("koala-ui-shape-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("echo.py"),
        "import json,sys\nr=json.load(sys.stdin)\nprint(json.dumps({'context':json.dumps(r)}))",
    )
    .unwrap();
    for version in [1, 2] {
        std::fs::write(root.join("extension.toml"), format!("api_version = {version}\nname = 'shape'\ncommand = ['python3', 'echo.py']\nhooks = ['before_model']\n")).unwrap();
        let host = load(
            &ExtensionsConfig {
                manifests: vec![root.join("extension.toml")],
                ..Default::default()
            },
            Vec::new(),
        )
        .unwrap();
        let response = host
            .hook(Stage::BeforeModel, serde_json::json!({}))
            .await
            .unwrap()
            .context
            .unwrap();
        let payload: serde_json::Value =
            serde_json::from_str(response.lines().find(|s| s.starts_with('{')).unwrap()).unwrap();
        if version == 1 {
            assert!(payload.get("capabilities").is_none());
            assert!(payload.get("session_id").is_none());
        } else {
            assert_eq!(payload["capabilities"]["ui"], false);
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn invalid_tool_response_never_publishes_ui() {
    use std::sync::Arc;
    struct MissingContent;
    impl Extension for MissingContent {
        fn name(&self) -> &str {
            "missing-content"
        }
        fn ui_enabled(&self) -> bool {
            true
        }
        fn tools(&self) -> Vec<Tool> {
            vec![Tool::function("bad", "Bad", serde_json::json!({}))]
        }
        fn hook<'a>(&'a self, _: Stage, _: &'a serde_json::Value) -> ExtensionFuture<'a> {
            Box::pin(async { Ok(Response::default()) })
        }
        fn execute<'a>(&'a self, _: &'a str, _: &'a serde_json::Value) -> ExtensionFuture<'a> {
            Box::pin(async {
                Ok(Response {
                    ui: vec![UiCommand::SetStatus {
                        id: "status".into(),
                        text: "should not be shown".into(),
                    }],
                    ..Default::default()
                })
            })
        }
    }
    let mut extensions = Extensions::default();
    extensions.register(Arc::new(MissingContent)).unwrap();
    let (_, tool) = extensions.tool_entries().pop().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let response = ui::scope(
        ui::RequestContext {
            ui: true,
            updates: Some(tx),
            ..Default::default()
        },
        tool.execute("bad", &serde_json::json!({})),
    )
    .await;
    assert!(rx.try_recv().is_err(), "invalid tool response changed UI");
    assert!(response.is_err());
}
