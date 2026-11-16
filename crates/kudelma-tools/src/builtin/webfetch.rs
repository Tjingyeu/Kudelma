use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use kudelma_permissions::Scope;
use regex::Regex;
use reqwest::header::{CONTENT_TYPE, LOCATION};
use serde_json::{Value, json};
use url::Url;

use crate::{
    Tool, ToolContext, ToolError, ToolOutput, ToolPermission, object_schema, optional_usize,
    required_str,
};

const MAX_REDIRECTS: usize = 5;
const FETCH_TIMEOUT_SECS: u64 = 30;

pub struct WebfetchTool;

#[async_trait]
impl Tool for WebfetchTool {
    fn name(&self) -> &'static str {
        "webfetch"
    }

    fn description(&self) -> &'static str {
        "Fetch an http(s) URL and return readable text. Private, loopback and metadata addresses are blocked unless explicitly allowed in net.allow_private_hosts."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "url": { "type": "string", "description": "http or https URL" },
                "max_chars": { "type": "integer", "description": "Maximum text characters to return (default 40000)" }
            }),
            &["url"],
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn permission(&self, args: &Value, ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        let url = required_str(args, "url")?;
        let _ = ctx;
        Ok(ToolPermission::new(vec![Scope::exact(url)]))
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let raw_url = required_str(&args, "url")?;
        let max_chars = optional_usize(&args, "max_chars")
            .unwrap_or(40_000)
            .clamp(500, 200_000);
        let (body, content_type, final_url, status) = fetch(&raw_url, &ctx.config).await?;
        let text = if is_html(&content_type, &body) {
            html_to_text(&body)
        } else {
            String::from_utf8_lossy(&body).to_string()
        };
        let text = text.trim();
        let mut clipped: String = text.chars().take(max_chars).collect();
        if text.chars().count() > max_chars {
            clipped.push_str("\n[... page truncated ...]");
        }
        let title = extract_title(&body);
        let mut output = format!("Fetched {final_url} (HTTP {status}, {content_type})\n");
        if let Some(title) = title {
            output.push_str(&format!("Title: {title}\n"));
        }
        output.push('\n');
        output.push_str(&clipped);
        Ok(ToolOutput::success(output))
    }
}

async fn fetch(
    raw_url: &str,
    config: &kudelma_config::Config,
) -> Result<(Vec<u8>, String, String, u16), ToolError> {
    let mut url = Url::parse(raw_url)
        .map_err(|error| ToolError::InvalidArgs(format!("invalid URL: {error}")))?;
    let client = client();
    for _ in 0..=MAX_REDIRECTS {
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(ToolError::InvalidArgs(
                "only http and https URLs are supported".into(),
            ));
        }
        ensure_public(
            url.host_str().unwrap_or_default(),
            url.port_or_known_default().unwrap_or(80),
            config,
        )
        .await?;
        let response = client
            .get(url.clone())
            .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
            .send()
            .await
            .map_err(|error| ToolError::Other(format!("fetch failed: {error}")))?;
        if response.status().is_redirection() {
            let Some(location) = response.headers().get(LOCATION) else {
                return Err(ToolError::Other("redirect without Location header".into()));
            };
            let location = location
                .to_str()
                .map_err(|error| ToolError::Other(error.to_string()))?;
            url = url
                .join(location)
                .map_err(|error| ToolError::Other(format!("invalid redirect: {error}")))?;
            continue;
        }
        if !response.status().is_success() {
            return Err(ToolError::Other(format!(
                "HTTP {} from {url}",
                response.status()
            )));
        }
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let limit = config.net.max_fetch_bytes;
        let mut response = response;
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| ToolError::Other(format!("read failed: {error}")))?
        {
            body.extend_from_slice(&chunk);
            if body.len() >= limit {
                body.truncate(limit);
                break;
            }
        }
        if !content_type.starts_with("text/")
            && !content_type.contains("json")
            && !content_type.contains("xml")
            && !content_type.contains("html")
        {
            return Err(ToolError::Other(format!(
                "unsupported content type: {content_type}"
            )));
        }
        return Ok((body, content_type, url.to_string(), status));
    }
    Err(ToolError::Other("too many redirects".into()))
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("kudelma/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("static client")
    })
}

async fn ensure_public(
    host: &str,
    port: u16,
    config: &kudelma_config::Config,
) -> Result<(), ToolError> {
    if host_allowed(host, &config.net.allow_private_hosts) {
        return Ok(());
    }
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| ToolError::Other(format!("cannot resolve {host}: {error}")))?;
    for address in addresses {
        if is_private(address.ip()) {
            return Err(ToolError::Other(format!(
                "refusing to fetch private address {} ({host}); add it to net.allow_private_hosts",
                address.ip()
            )));
        }
    }
    Ok(())
}

fn host_allowed(host: &str, patterns: &[String]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    patterns.iter().any(|pattern| {
        let pattern = pattern.trim_end_matches('.').to_ascii_lowercase();
        match pattern.strip_prefix("*.") {
            Some(domain) => host.ends_with(&format!(".{domain}")),
            None => host == pattern,
        }
    })
}

fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_unspecified()
                || ip.is_multicast()
                || octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 198 && (18..=19).contains(&octets[1]))
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip
                    .to_ipv4_mapped()
                    .map(|v4| is_private(IpAddr::V4(v4)))
                    .unwrap_or(false)
        }
    }
}

fn is_html(content_type: &str, body: &[u8]) -> bool {
    if content_type.contains("html") {
        return true;
    }
    let prefix = String::from_utf8_lossy(&body[..body.len().min(128)]).to_ascii_lowercase();
    prefix.contains("<html") || prefix.contains("<!doctype html")
}

fn html_to_text(body: &[u8]) -> String {
    let html = String::from_utf8_lossy(body);
    let mut text = html.to_string();
    text = script_regex().replace_all(&text, " ").to_string();
    text = style_regex().replace_all(&text, " ").to_string();
    text = block_regex().replace_all(&text, "\n").to_string();
    text = tag_regex().replace_all(&text, "").to_string();
    text = html_escape::decode_html_entities(&text).to_string();
    let mut output = String::new();
    let mut blank = 0;
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        output.push_str(&line);
        output.push('\n');
    }
    output.trim().to_string()
}

fn extract_title(body: &[u8]) -> Option<String> {
    let html = String::from_utf8_lossy(body);
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let start = lower[start..].find('>')? + start + 1;
    let end = lower[start..].find("</title>")? + start;
    let title = html_escape::decode_html_entities(html[start..end].trim()).to_string();
    (!title.is_empty()).then_some(title)
}

fn script_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>").expect("static regex"))
}

fn style_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?is)<style\b[^>]*>.*?</style\s*>").expect("static regex"))
}

fn block_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)</?(p|div|section|article|li|tr|h[1-6]|br|pre|blockquote)[^>]*>")
            .expect("static regex")
    })
}

fn tag_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?s)<[^>]+>").expect("static regex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_addresses_are_blocked() {
        assert!(is_private("127.0.0.1".parse().unwrap()));
        assert!(is_private("10.0.0.1".parse().unwrap()));
        assert!(is_private("169.254.169.254".parse().unwrap()));
        assert!(is_private("::1".parse().unwrap()));
        assert!(!is_private("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn html_becomes_readable_text() {
        let text =
            html_to_text(b"<html><script>x()</script><h1>Title</h1><p>Hello&nbsp;world</p></html>");
        assert!(text.contains("Title"));
        assert!(text.contains("Hello"));
        assert!(!text.contains("x()"));
    }

    #[test]
    fn wildcard_host_patterns_exclude_the_bare_domain() {
        assert!(host_allowed("api.example.com", &["*.example.com".into()]));
        assert!(!host_allowed("example.com", &["*.example.com".into()]));
        assert!(host_allowed("example.com", &["example.com".into()]));
    }
}
