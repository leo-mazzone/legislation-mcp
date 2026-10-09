mod client;
mod xml;

use client::{identifier, provision, LegislationClient};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars, tool, tool_handler, tool_router, ServerHandler,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Search {
    /// Words to search in the title. Supply title and/or text.
    pub title: Option<String>,
    /// Words to search in the legislation text; this is keyword search, not semantic search.
    pub text: Option<String>,
    /// Legislation type such as ukpga, uksi, asp, or all (default).
    pub legislation_type: Option<String>,
    pub year: Option<u16>,
    /// One-based page, defaults to 1.
    pub page: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Item {
    /// Identifier returned by search, e.g. ukpga/2010/15.
    pub identifier: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Provision {
    pub identifier: String,
    /// Path from contents, e.g. section/20 or schedule/1/paragraph/2.
    pub provision: String,
}

#[derive(Clone)]
pub struct LegislationServer {
    client: LegislationClient,
    tool_router: ToolRouter<Self>,
}

fn result(value: Result<Value, String>) -> CallToolResult {
    match value {
        Ok(value) => {
            let mut response = CallToolResult::success(vec![Content::text(value.to_string())]);
            response.structured_content = Some(value);
            response
        }
        Err(message) => {
            CallToolResult::error(vec![Content::text(json!({"error": message}).to_string())])
        }
    }
}

#[tool_router]
impl LegislationServer {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: LegislationClient::new()?,
            tool_router: Self::tool_router(),
        })
    }

    #[tool(
        description = "Search UK legislation by title or text. Returns identifiers and source links. Use contents and provision tools to retrieve evidence before explaining the law."
    )]
    async fn search_legislation(
        &self,
        Parameters(args): Parameters<Search>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(result(self.search(args).await))
    }

    #[tool(
        description = "Get the latest available table of contents for legislation. Returns provision paths, headings, source metadata and amendment notices."
    )]
    async fn get_legislation_contents(
        &self,
        Parameters(args): Parameters<Item>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(result(
            self.retrieve(&args.identifier, "contents", true).await,
        ))
    }

    #[tool(
        description = "Read a specific legislation provision in its latest available version. Cite source_url and retain amendment notices. Latest available text does not establish commencement or complete updating."
    )]
    async fn get_legislation_provision(
        &self,
        Parameters(args): Parameters<Provision>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let value = match provision(&args.provision) {
            Ok(path) => self.retrieve(&args.identifier, path, false).await,
            Err(error) => Err(error),
        };
        Ok(result(value))
    }
}

impl LegislationServer {
    async fn search(&self, args: Search) -> Result<Value, String> {
        let title = args.title.as_deref().unwrap_or("").trim();
        let text = args.text.as_deref().unwrap_or("").trim();
        if (title.is_empty() && text.is_empty()) || title.len() > 500 || text.len() > 500 {
            return Err("invalid_input: supply title and/or text, at most 500 bytes each".into());
        }
        let kind = args.legislation_type.as_deref().unwrap_or("all");
        if kind.is_empty() || kind.len() > 12 || !kind.bytes().all(|byte| byte.is_ascii_lowercase())
        {
            return Err("invalid_input: use a legislation type such as ukpga, uksi, or all".into());
        }
        let page = args.page.unwrap_or(1);
        if page == 0
            || page > 10_000
            || args.year.is_some_and(|year| !(1000..=9999).contains(&year))
        {
            return Err("invalid_input: invalid page or year".into());
        }
        let path = match args.year {
            Some(year) => format!("{kind}/{year}/data.feed"),
            None => format!("{kind}/data.feed"),
        };
        let mut url = self.client.url(&path)?;
        {
            let mut query = url.query_pairs_mut();
            if !title.is_empty() {
                query.append_pair("title", title);
            }
            if !text.is_empty() {
                query.append_pair("text", text);
            }
            query
                .append_pair("page", &page.to_string())
                .append_pair("results-count", "10");
        }
        let root = xml::parse(&self.client.get(url.clone()).await?)?;
        let mut response = xml::search(&root)?;
        response["page"] = json!(page);
        response["source_url"] = json!(url.as_str());
        Ok(response)
    }

    async fn retrieve(&self, id: &str, path: &str, contents: bool) -> Result<Value, String> {
        let id = identifier(id)?;
        let url = self.client.url(&format!("{id}/{path}/data.xml"))?;
        let root = xml::parse(&self.client.get(url).await?)?;
        let source = format!("https://www.legislation.gov.uk/{id}/{path}");
        let mut response = xml::document(&root, &source, contents)?;
        response["identifier"] = json!(id);
        // Bound the combined response, including contents and metadata.
        if response.to_string().len() > 512_000 {
            return Err(format!(
                "response_too_large: open {source} or retrieve a narrower provision"
            ));
        }
        Ok(response)
    }
}

#[tool_handler]
impl ServerHandler for LegislationServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            instructions: Some("Read-only access to legislation.gov.uk. Retrieve provisions before answering; cite exact source links and preserve version and amendment warnings. Search is keyword-based. Do not infer commencement from latest available text.".into()),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::ServiceExt;

    #[tokio::test]
    async fn equality_act_contents_fit_response_budget() {
        use axum::{routing::get, Router};
        // Captured from legislation.gov.uk: 1,014 contents entries exceed 150 KB.
        let app = Router::new().route(
            "/ukpga/2010/15/contents/data.xml",
            get(|| async {
                (
                    [("content-type", "application/xml")],
                    include_str!("../tests/fixtures/equality-contents.xml"),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut server = LegislationServer::new().unwrap();
        server.client =
            LegislationClient::for_test(reqwest::Url::parse(&format!("http://{addr}/")).unwrap());
        let response = server
            .retrieve("ukpga/2010/15", "contents", true)
            .await
            .unwrap();
        let contents = response["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1014);
        assert!(contents
            .iter()
            .any(|item| item["provision"] == "section/20"));
        assert!(!response["metadata"].as_array().unwrap().is_empty());
        assert!(!response["outstanding_amendments"]
            .as_array()
            .unwrap()
            .is_empty());
        let bytes = response.to_string().len();
        assert!(
            bytes > 150_000 && bytes <= 512_000,
            "response bytes: {bytes}"
        );
        upstream.abort();
    }

    #[tokio::test]
    async fn mcp_initialization_discovery_and_error_call() {
        use axum::{routing::get, Router};
        let app = Router::new().route(
            "/ukpga/2010/15/section/20/data.xml",
            get(|| async {
                (
                    [("content-type", "application/xml")],
                    include_str!("../tests/fixtures/provision.xml"),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut server = LegislationServer::new().unwrap();
        server.client =
            LegislationClient::for_test(reqwest::Url::parse(&format!("http://{addr}/")).unwrap());
        let (server_io, client_io) = tokio::io::duplex(65536);
        let task = tokio::spawn(async move {
            server
                .serve(server_io)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let client = ().serve(client_io).await.unwrap();
        assert!(client.peer_info().unwrap().capabilities.tools.is_some());
        let tools = client.list_tools(Default::default()).await.unwrap();
        assert_eq!(tools.tools.len(), 3);
        let response = client
            .call_tool(CallToolRequestParam {
                name: "get_legislation_provision".into(),
                arguments: json!({"identifier": "../bad", "provision": "section/20"})
                    .as_object()
                    .cloned(),
            })
            .await
            .unwrap();
        assert_eq!(response.is_error, Some(true));
        let success = client
            .call_tool(CallToolRequestParam {
                name: "get_legislation_provision".into(),
                arguments: json!({"identifier":"ukpga/2010/15", "provision":"section/20"})
                    .as_object()
                    .cloned(),
            })
            .await
            .unwrap();
        assert_ne!(success.is_error, Some(true));
        assert!(success.structured_content.unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("reasonable adjustment"));
        client.cancel().await.unwrap();
        task.await.unwrap();
        upstream.abort();
    }

    #[test]
    fn successful_results_include_structured_data() {
        let response = result(Ok(
            json!({"text": "source passage", "source_url": "source"}),
        ));
        assert_eq!(
            response.structured_content.unwrap()["text"],
            "source passage"
        );
        assert_ne!(response.is_error, Some(true));
    }
}
