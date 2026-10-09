use reqwest::{redirect::Policy, Client, Url};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};

const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_CACHE_ITEMS: usize = 16;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const REQUEST_SLOTS: usize = 4;
type Cache = HashMap<String, (Instant, String)>;

#[derive(Clone)]
pub struct LegislationClient {
    client: Client,
    base: Url,
    cache: Arc<Mutex<Cache>>,
    slots: Arc<Semaphore>,
}

impl LegislationClient {
    #[cfg(test)]
    pub(crate) fn for_test(base: Url) -> Self {
        let mut client = Self::new().unwrap();
        client.base = base;
        client.client = Client::builder().redirect(Policy::none()).build().unwrap();
        client
    }

    pub fn new() -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            .user_agent("govchat-legislation-mcp/0.1 (local demo)")
            .timeout(REQUEST_TIMEOUT)
            .redirect(Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    return attempt.error("too many redirects");
                }
                if attempt.url().scheme() == "https"
                    && attempt.url().host_str() == Some("www.legislation.gov.uk")
                {
                    attempt.follow()
                } else {
                    attempt.error("redirect outside legislation.gov.uk")
                }
            }))
            .build()?;
        Ok(Self {
            client,
            base: Url::parse("https://www.legislation.gov.uk/").unwrap(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            slots: Arc::new(Semaphore::new(REQUEST_SLOTS)),
        })
    }

    pub fn url(&self, path: &str) -> Result<Url, String> {
        self.base
            .join(path)
            .map_err(|_| "invalid_input: invalid request path".into())
    }

    pub async fn get(&self, url: Url) -> Result<String, String> {
        if url.origin() != self.base.origin() {
            return Err("invalid_input: external URLs are not accepted".into());
        }
        if let Some((at, xml)) = self.cache.lock().await.get(url.as_str()) {
            if at.elapsed() < Duration::from_secs(300) {
                return Ok(xml.clone());
            }
        }
        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|_| "unavailable: service is stopping".to_string())?;
        let mut response = self.client.get(url.clone()).send().await.map_err(|error| {
            if error.is_timeout() {
                format!(
                    "timeout: legislation.gov.uk did not respond within {} seconds",
                    REQUEST_TIMEOUT.as_secs()
                )
            } else {
                "upstream_error: could not reach legislation.gov.uk".to_string()
            }
        })?;
        match response.status().as_u16() {
            200 => {},
            404 | 410 => return Err("not_found: document or provision is unavailable; check the identifier and contents".into()),
            403 | 429 => return Err("rate_limited_or_denied: legislation.gov.uk refused this request; wait before retrying".into()),
            503 | 504 => return Err("upstream_unavailable: wait at least five minutes before retrying".into()),
            300 => return Err("ambiguous_document: open the source URL to select the correct item".into()),
            400 => return Err("invalid_request: legislation.gov.uk rejected this identifier or search".into()),
            _ => return Err("upstream_error: legislation.gov.uk returned an unsuccessful response".into()),
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if !content_type.contains("xml") && !content_type.contains("atom") {
            return Err("pdf_only_or_unavailable: XML was not returned; open the source URL for available formats".into());
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_BYTES as u64)
        {
            return Err("response_too_large: retrieve a narrower provision".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "upstream_error: could not read the response".to_string())?
        {
            if bytes.len() + chunk.len() > MAX_BYTES {
                return Err("response_too_large: retrieve a narrower provision".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let xml =
            String::from_utf8(bytes).map_err(|_| "invalid_xml: expected UTF-8 XML".to_string())?;
        let mut cache = self.cache.lock().await;
        cache.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(300));
        // Bound memory to at most sixteen upstream documents (64 MiB).
        if cache.len() >= MAX_CACHE_ITEMS && !cache.contains_key(url.as_str()) {
            let oldest = cache
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(key, _)| key.clone());
            if let Some(key) = oldest {
                cache.remove(&key);
            }
        }
        cache.insert(url.to_string(), (Instant::now(), xml.clone()));
        Ok(xml)
    }
}

/// Validate the relative path identifying a whole piece of legislation.
///
/// Accepts `type/year/number` (e.g. `ukpga/2010/15`) or the historical
/// `type/reign/session/number` form (e.g. `ukpga/Edw7/2/41`). The type must be
/// 1–12 lowercase ASCII letters; remaining segments must be 1–16 ASCII letters
/// or digits. The year, session and final number must contain only digits.
///
/// These checks reject full URLs, empty segments, query strings and `..` path
/// traversal before the value is used to build an upstream URL. They validate
/// the format only: they do not check whether the type or legislation exists.
/// Returns the original borrowed string unchanged, or an `invalid_input` error.
pub fn identifier(value: &str) -> Result<&str, String> {
    let parts: Vec<_> = value.split('/').collect();
    let numeric = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let valid = parts.len() >= 3
        && parts.len() <= 4
        && !parts[0].is_empty()
        && parts[0].bytes().all(|byte| byte.is_ascii_lowercase())
        && parts[0].len() <= 12
        && parts[1..].iter().all(|part| {
            !part.is_empty()
                && part.len() <= 16
                && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        && numeric(parts[parts.len() - 1])
        && ((parts.len() == 3 && numeric(parts[1])) || (parts.len() == 4 && numeric(parts[2])));
    if valid {
        Ok(value)
    } else {
        Err("invalid_input: use an identifier such as ukpga/2010/15 or ukpga/Edw7/2/41".into())
    }
}

/// Validate the relative path selecting a provision within legislation.
///
/// Examples include `section/20` and `schedule/1/paragraph/2`. Accepts 2–8
/// slash-separated segments, starting with `section`, `regulation`, `article`,
/// `schedule`, `part`, `chapter` or `rule`. Each remaining segment must contain
/// 1–32 ASCII letters, digits or hyphens; this allows labels such as `20A`.
///
/// These checks reject full URLs, empty segments, query strings and `..` path
/// traversal. They do not check whether the provision exists or whether its
/// nested path describes a valid hierarchy in the selected legislation.
/// Returns the original borrowed string unchanged, or an `invalid_input` error.
pub fn provision(value: &str) -> Result<&str, String> {
    let parts: Vec<_> = value.split('/').collect();
    let valid = parts.len() >= 2
        && parts.len() <= 8
        && matches!(
            parts[0],
            "section" | "regulation" | "article" | "schedule" | "part" | "chapter" | "rule"
        )
        && parts[1..].iter().all(|part| {
            !part.is_empty()
                && part.len() <= 32
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    if valid {
        Ok(value)
    } else {
        Err(
            "invalid_input: use a provision path such as section/20 or schedule/1/paragraph/2"
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};
    async fn mock(
        status: u16,
        body: &'static str,
        delay: bool,
    ) -> (LegislationClient, tokio::task::JoinHandle<()>) {
        let app = Router::new().route(
            "/data.xml",
            get(move || async move {
                if delay {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    [("content-type", "application/xml")],
                    body,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut client = LegislationClient::new().unwrap();
        client.base = Url::parse(&format!("http://{addr}/")).unwrap();
        client.client = Client::builder()
            .timeout(Duration::from_millis(30))
            .redirect(Policy::none())
            .build()
            .unwrap();
        (client, task)
    }
    #[tokio::test]
    async fn upstream_failures_are_distinct() {
        for (status, code) in [
            (404, "not_found"),
            (429, "rate_limited_or_denied"),
            (503, "upstream_unavailable"),
        ] {
            let (client, task) = mock(status, "", false).await;
            assert!(client
                .get(client.url("data.xml").unwrap())
                .await
                .unwrap_err()
                .starts_with(code));
            task.abort();
        }
        let (client, task) = mock(200, "<Legislation/>", true).await;
        assert!(client
            .get(client.url("data.xml").unwrap())
            .await
            .unwrap_err()
            .starts_with("timeout"));
        task.abort();
    }
    #[tokio::test]
    async fn cache_and_origin_validation() {
        let (client, task) = mock(200, "<Legislation/>", false).await;
        let url = client.url("data.xml").unwrap();
        assert_eq!(client.get(url.clone()).await.unwrap(), "<Legislation/>");
        task.abort();
        assert_eq!(client.get(url).await.unwrap(), "<Legislation/>");
        assert!(client
            .get(Url::parse("http://example.com/data.xml").unwrap())
            .await
            .is_err());
    }
    #[tokio::test]
    async fn full_cache_preserves_newer_documents() {
        let (client, task) = mock(200, "<Legislation/>", false).await;
        let urls: Vec<_> = (0..MAX_CACHE_ITEMS)
            .map(|index| client.url(&format!("data.xml?item={index}")).unwrap())
            .collect();
        let now = Instant::now();
        {
            let mut cache = client.cache.lock().await;
            for (index, url) in urls.iter().enumerate() {
                cache.insert(
                    url.to_string(),
                    (
                        now - Duration::from_secs((MAX_CACHE_ITEMS - index) as u64),
                        "<Cached/>".into(),
                    ),
                );
            }
        }
        let new_url = client.url("data.xml").unwrap();
        client.get(new_url.clone()).await.unwrap();
        {
            let cache = client.cache.lock().await;
            assert_eq!(cache.len(), MAX_CACHE_ITEMS);
            assert!(!cache.contains_key(urls[0].as_str()));
            assert!(cache.contains_key(new_url.as_str()));
        }
        task.abort();
        for url in urls.iter().skip(1) {
            assert_eq!(client.get(url.clone()).await.unwrap(), "<Cached/>");
        }
    }
    #[test]
    fn reject_untrusted_paths() {
        for value in [
            "../2010/15",
            "https://example.com",
            "ukpga/2010/15?x=1",
            "ukpga//15",
        ] {
            assert!(identifier(value).is_err());
        }
        assert!(identifier("ukpga/Edw7/2/41").is_ok());
        assert!(provision("section/20").is_ok());
        assert!(provision("section/../20").is_err());
    }
}
