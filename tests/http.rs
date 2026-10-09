use axum::{routing::get, Router};
use rmcp::{
    model::CallToolRequestParam,
    transport::{
        streamable_http_server::{session::local::LocalSessionManager, StreamableHttpService},
        StreamableHttpClientTransport,
    },
    ServiceExt,
};
use serde_json::json;
use std::sync::Arc;
use uk_legislation_mcp::LegislationServer;

#[tokio::test]
async fn streamable_http_connection_and_tool_discovery() {
    let server = LegislationServer::new().unwrap();
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        Default::default(),
    );
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("http://{addr}/mcp");
    let client = ().serve(StreamableHttpClientTransport::from_uri(url)).await.unwrap();
    assert_eq!(
        client
            .list_tools(Default::default())
            .await
            .unwrap()
            .tools
            .len(),
        3
    );
    let response = client
        .call_tool(CallToolRequestParam {
            name: "search_legislation".into(),
            arguments: json!({"title":""}).as_object().cloned(),
        })
        .await
        .unwrap();
    assert_eq!(response.is_error, Some(true));
    client.cancel().await.unwrap();
    task.abort();
}
