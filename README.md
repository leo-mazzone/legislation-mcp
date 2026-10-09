# UK legislation MCP

Read-only Rust MCP server for UK legislation. It searches legislation.gov.uk, retrieves tables of contents and reads individual provisions. Responses contain JSON as both MCP structured content and text, with source links and available metadata.

## Start the service

```sh
docker compose up --build -d
curl --fail http://localhost:8090/health
```

The service exposes Streamable HTTP at `http://localhost:8090/mcp`.

Alternatively, run `cargo run` to serve on `127.0.0.1:8080`. Set `BIND_ADDR` to change the listening address. The Compose setup sets it to `0.0.0.0:8080` inside the container.

## Tools

| Tool | Arguments | Output |
| --- | --- | --- |
| `search_legislation` | `title` and/or `text`; optional `legislation_type`, `year`, `page` | Up to 10 matches, identifiers, links, update dates and next-page URL |
| `get_legislation_contents` | `identifier`, e.g. `ukpga/2010/15` | Hierarchical headings, numbering, references and provision paths |
| `get_legislation_provision` | `identifier`, `provision`, e.g. `section/20` | Readable provision text, numbering, table cells and source metadata |

Identifiers also support historical regnal-year identifiers such as `ukpga/Edw7/2/41`. Use the next page number with the same search parameters for pagination. Search is keyword-based, not semantic retrieval. All retrieval uses the latest available version; historical versions and detailed commencement analysis are not yet implemented.

The client sends a User-Agent, limits concurrent requests to four, caches up to sixteen documents for five minutes, times out requests after twenty seconds and limits each upstream response to 4 MiB. Provision text is capped at 30,000 characters with an explicit `truncated` flag. Combined document results are capped at 150 KB. Errors use MCP `isError`, with a readable code and message. There are no automatic retries.

## Checks

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo test --test live -- --ignored
```

Normal tests use synthetic fixtures and local HTTP mocks; the live test calls the official API through MCP. Try these prompts after connecting to a harness:

1. Find the Equality Act 2010 and show its relevant contents.
2. Explain section 20, quoting a short passage and linking to the exact section.
3. Compare sections 20 and 21, citing both provisions.

Success means that the agent calls the tools, explains retrieved text, cites exact source links and retains relevant warnings. Latest available text is not a guarantee that every amendment has been applied or that a provision is in force.