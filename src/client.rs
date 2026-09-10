//! Blocking HTTP client for the EspoCRM REST API.
//! Maps HTTP failures to ApiError so main can translate them into exit codes.

use anyhow::{Context, Result};
use reqwest::blocking::Client as Http;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

pub const EXIT_ERROR: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_AUTH: u8 = 3;
pub const EXIT_NOT_FOUND: u8 = 4;

#[derive(Debug)]
pub struct ApiError {
    pub code: u8,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

pub fn usage(message: impl Into<String>) -> anyhow::Error {
    ApiError { code: EXIT_USAGE, message: message.into() }.into()
}

pub struct Client {
    http: Http,
    base: String,
    api_key: String,
    pub dry_run: bool,
}

impl Client {
    pub fn new(url: &str, api_key: &str, dry_run: bool) -> Result<Self> {
        let url = url.trim_end_matches('/');
        if let Some(rest) = url.strip_prefix("http://") {
            // The API key travels in a header; over plaintext it is exposed on the wire.
            let host = rest.split(['/', ':']).next().unwrap_or("");
            if !matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1") {
                return Err(usage(format!(
                    "refusing plaintext http:// to {host}: the api key would be sent unencrypted; use https://"
                )));
            }
        } else if !url.starts_with("https://") {
            return Err(usage(format!("url must start with https://, got {url:?}")));
        }
        let base = if url.ends_with("/api/v1") { url.to_string() } else { format!("{url}/api/v1") };
        let http = Http::builder()
            .timeout(std::time::Duration::from_secs(
                std::env::var("ESPO_HTTP_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(60),
            ))
            .user_agent(concat!("espocli/", env!("CARGO_PKG_VERSION")))
            // reqwest strips Authorization on a cross-host redirect but not X-Api-Key.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building http client")?;
        Ok(Self { http, base, api_key: api_key.to_string(), dry_run })
    }

    /// Under --dry-run the request is printed instead of sent, and Value::Null comes back.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        extra: &[(&str, &str)],
    ) -> Result<Value> {
        if self.dry_run {
            let url = format!("{}/{}", self.base, path.trim_start_matches('/'));
            let qs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let sep = if qs.is_empty() { "" } else { "?" };
            println!("{method} {url}{sep}{}", qs.join("&"));
            if let Some(b) = body {
                println!("{b}");
            }
            return Ok(Value::Null);
        }
        self.send(method, path, query, body, extra)
    }

    /// Always performs the call: metadata is needed to build the request that --dry-run describes.
    pub fn send(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        extra: &[(&str, &str)],
    ) -> Result<Value> {
        let url = format!("{}/{}", self.base, path.trim_start_matches('/'));
        let verb = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| usage(format!("invalid HTTP method {method:?}")))?;
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Api-Key",
            HeaderValue::from_str(&self.api_key).map_err(|_| usage("api key has invalid characters"))?,
        );
        for (k, v) in extra {
            let name = HeaderName::from_bytes(k.as_bytes()).map_err(|_| usage(format!("bad header {k}")))?;
            headers.insert(name, HeaderValue::from_str(v).map_err(|_| usage(format!("bad value for {k}")))?);
        }

        let mut req = self.http.request(verb, &url).headers(headers).query(query);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().with_context(|| format!("requesting {url}"))?;
        let status = resp.status();
        let reason = resp
            .headers()
            .get("x-status-reason")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = resp.text().unwrap_or_default();

        if !status.is_success() {
            let code = match status.as_u16() {
                401 | 403 => EXIT_AUTH,
                404 => EXIT_NOT_FOUND,
                _ => EXIT_ERROR,
            };
            let detail = first_non_empty([reason.as_str(), text.trim(), status.canonical_reason().unwrap_or("")]);
            return Err(ApiError { code, message: format!("{} {method} {path}: {detail}", status.as_u16()) }.into());
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).with_context(|| format!("parsing response from {url}"))
    }
}

fn first_non_empty<'a, const N: usize>(candidates: [&'a str; N]) -> &'a str {
    candidates.into_iter().find(|s| !s.is_empty()).unwrap_or("request failed")
}
