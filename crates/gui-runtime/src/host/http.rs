//! HTTP egress: the request/response types, the status-code table, and the allowlisted +
//! call-capped `fetch`. The allowlist and limits live on the host; this module holds only the
//! per-run call counter.

use std::io::Read;
use std::time::Duration;

use gui_schema::{Capabilities, Limits};
use serde_json::{Map, Value};

use super::HostError;

/// An outbound HTTP request a handler asked for.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

impl HttpRequest {
    /// Build a request from a guest-supplied `{url, method?, headers?, body?}` value.
    pub fn from_json(v: &Value) -> Result<Self, HostError> {
        let url = v
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| HostError::Other("http request missing url".into()))?
            .to_string();
        let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("GET").to_string();
        let headers = v
            .get("headers")
            .and_then(|h| h.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let body = match v.get("body") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Null) | None => None,
            Some(other) => Some(other.to_string()),
        };
        Ok(Self { url, method, headers, body })
    }
}

/// The response handed back to the guest.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    /// As a JSON object `{status, headers, body}` for handing back to the guest.
    pub fn to_json(&self) -> Value {
        let headers: Map<String, Value> = self
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        serde_json::json!({ "status": self.status, "headers": headers, "body": self.body })
    }
}

/// `NAME -> code` for every status the `http` crate knows, named in SCREAMING_SNAKE to match
/// the crate's own constants (e.g. 404 → `NOT_FOUND`, 200 → `OK`, 418 → `IM_A_TEAPOT`).
pub fn status_codes() -> Map<String, Value> {
    (100u16..=599)
        .filter_map(|n| {
            let s = http::StatusCode::from_u16(n).ok()?;
            let reason = s.canonical_reason()?;
            Some((screaming_snake(reason), Value::from(n)))
        })
        .collect()
}

fn screaming_snake(reason: &str) -> String {
    reason
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join("_")
}

/// HTTP egress state: just the per-run call counter (allowlist + limits are passed in by the host).
#[derive(Default)]
pub struct Http {
    calls: u32,
}

impl Http {
    pub fn fetch(
        &mut self,
        req: HttpRequest,
        caps: &Capabilities,
        limits: &Limits,
    ) -> Result<HttpResponse, HostError> {
        let host = url::Url::parse(&req.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .ok_or_else(|| HostError::Other(format!("invalid url {:?}", req.url)))?;
        if !caps.http.iter().any(|h| h == &host) {
            return Err(HostError::Denied(format!("http host {host:?} not in allowlist")));
        }
        if self.calls >= limits.http_calls {
            return Err(HostError::Limit(format!("http_calls exceeded ({})", limits.http_calls)));
        }
        self.calls += 1;

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(limits.wall_ms.max(1)))
            .build();
        let mut request = agent.request(&req.method, &req.url);
        for (k, v) in &req.headers {
            request = request.set(k, v);
        }
        let send = match &req.body {
            Some(b) => request.send_string(b),
            None => request.call(),
        };
        // ureq treats non-2xx as Err(Status); we want the response either way.
        let resp = match send {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(HostError::Other(format!("http error: {e}"))),
        };
        let status = resp.status();
        let headers = resp
            .headers_names()
            .into_iter()
            .filter_map(|n| resp.header(&n).map(|v| (n.clone(), v.to_string())))
            .collect();
        let cap = (limits.resp_kb as usize).saturating_mul(1024);
        let mut body = String::new();
        resp.into_reader()
            .take(cap as u64 + 1)
            .read_to_string(&mut body)
            .map_err(|e| HostError::Other(format!("read body: {e}")))?;
        if body.len() > cap {
            return Err(HostError::Limit(format!("response body exceeds {} KB", limits.resp_kb)));
        }
        Ok(HttpResponse { status, headers, body })
    }
}
