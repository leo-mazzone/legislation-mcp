//! Opt-in upstream checks; excluded from normal offline test runs.
use rmcp::{model::CallToolRequestParam, ServiceExt};
use serde_json::{json, Value};
use uk_legislation_mcp::LegislationServer;

#[tokio::test]
#[ignore = "requires access to legislation.gov.uk"]
async fn equality_act_search_contents_and_section() {
    let (server_io, client_io) = tokio::io::duplex(1_048_576);
    let task = tokio::spawn(async move {
        LegislationServer::new()
            .unwrap()
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let client = ().serve(client_io).await.unwrap();
    for (name, args, key) in [
        (
            "search_legislation",
            json!({"title":"Equality Act", "year":2010, "legislation_type":"ukpga"}),
            "results",
        ),
        (
            "get_legislation_contents",
            json!({"identifier":"ukpga/2010/15"}),
            "contents",
        ),
        (
            "get_legislation_provision",
            json!({"identifier":"ukpga/2010/15", "provision":"section/20"}),
            "text",
        ),
    ] {
        let response = client
            .call_tool(CallToolRequestParam {
                name: name.into(),
                arguments: args.as_object().cloned(),
            })
            .await
            .unwrap();
        assert_ne!(response.is_error, Some(true), "{response:?}");
        let value: Value = response.structured_content.unwrap();
        assert!(!value[key].is_null());
        if key == "text" {
            assert!(value[key].as_str().unwrap().contains("adjustments"));
        } else {
            assert!(!value[key].as_array().unwrap().is_empty());
        }
    }
    client.cancel().await.unwrap();
    task.await.unwrap();
}
