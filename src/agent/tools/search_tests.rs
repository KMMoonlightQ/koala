use crate::{
    agent::{
        Agent, event,
        tools::{ToolCatalog, ToolContext},
    },
    config::Config,
};
use serde_json::{Value, json};

#[tokio::test]
async fn plan_agents_can_discover_search_and_read_without_shell() {
    let root = std::env::temp_dir().join(format!("koala-search-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/中文.rs"), "first\nneedle 中文\n").unwrap();
    std::fs::write(root.join(".hidden.rs"), "needle").unwrap();
    std::fs::write(root.join("binary.rs"), b"needle\0binary").unwrap();
    let mut cfg = Config::default();
    cfg.llm.model = "test".into();
    cfg.agent.memory_file = root.join("memory.json");
    let mut agent = Agent::new(&cfg).await.unwrap();
    let events = event::null_events();
    for depth in [0, 1] {
        let catalog = ToolCatalog::build(depth, &agent.shared.extensions);
        let mut ctx = ToolContext {
            graph: None,
            todos: &mut agent.todos,
            agent_memory: &agent.agent_memory,
            background: agent.background.clone(),
            skills: &agent.skills,
            events: &events,
            shared: &agent.shared,
            depth,
            plan_mode: true,
        };
        for name in ["glob", "grep"] {
            assert!(
                catalog
                    .definitions_for_mode(true)
                    .iter()
                    .any(|t| t.function.name == name),
                "missing {name}"
            );
        }
        let found = catalog
            .execute(&mut ctx, "glob", json!({"path":root,"pattern":"**/*.rs"}))
            .await;
        assert!(!found.is_error, "{}", found.content);
        let found: Value = serde_json::from_str(&found.content).unwrap();
        assert!(
            found["results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p.as_str().unwrap().ends_with("src/中文.rs"))
        );
        assert!(!found.to_string().contains(".hidden.rs"));
        let matches = catalog
            .execute(
                &mut ctx,
                "grep",
                json!({"path":root,"pattern":"needle","glob":"**/*.rs"}),
            )
            .await;
        assert!(!matches.is_error, "{}", matches.content);
        let matches: Value = serde_json::from_str(&matches.content).unwrap();
        assert_eq!(matches["results"].as_array().unwrap().len(), 1);
        assert_eq!(matches["results"][0]["line"], 2);
        assert_eq!(matches["results"][0]["text"], "needle 中文");
        let limited = catalog
            .execute(
                &mut ctx,
                "glob",
                json!({"path":root,"pattern":"**/*.rs","limit":1}),
            )
            .await;
        let limited: Value = serde_json::from_str(&limited.content).unwrap();
        assert_eq!(limited["results"].as_array().unwrap().len(), 1);
        assert_eq!(limited["truncated"], true);
        let hidden = catalog.execute(&mut ctx, "grep", json!({"path":root,"pattern":"NEEDLE","hidden":true,"ignore_case":true,"literal":true})).await;
        let hidden: Value = serde_json::from_str(&hidden.content).unwrap();
        assert_eq!(hidden["results"].as_array().unwrap().len(), 2);
        assert_eq!(hidden["skipped_files"], 1);
        let no_matches = catalog
            .execute(
                &mut ctx,
                "grep",
                json!({"path":root.join("src/中文.rs"),"pattern":"needle.*","literal":true}),
            )
            .await;
        let no_matches: Value = serde_json::from_str(&no_matches.content).unwrap();
        assert_eq!(no_matches["results"], json!([]));
        assert_eq!(no_matches["truncated"], false);
        assert!(
            catalog
                .execute(
                    &mut ctx,
                    "glob",
                    json!({"path":root.join("missing"),"pattern":"**/*"})
                )
                .await
                .is_error
        );
        let read = catalog
            .execute(
                &mut ctx,
                "read",
                json!({"path":matches["results"][0]["path"],"offset":2}),
            )
            .await;
        assert!(!read.is_error);
        assert!(read.content.contains("needle 中文"));
        for args in [
            json!({"pattern":"["}),
            json!({"pattern":"x","limit":0}),
            json!({"pattern":"x","unexpected":true}),
        ] {
            assert!(catalog.execute(&mut ctx, "grep", args).await.is_error);
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}
