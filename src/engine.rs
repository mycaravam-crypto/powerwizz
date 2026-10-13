use std::time::{Duration, Instant};

use reqwest::Url;

use crate::model::{Method, Step, resolve_url};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ErrorKind {
    /// Response arrived but its status was not expected.
    Status,
    Timeout,
    Connect,
    Other,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Status => "status",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Connect => "connect",
            ErrorKind::Other => "other",
        }
    }
}

/// One executed request. Holds no credentials, cookies, headers, bodies or query strings.
#[derive(Clone, Debug)]
pub struct RequestRecord {
    /// Time since test start when the request began.
    pub at: Duration,
    pub vu: u32,
    /// "scenario/step"
    pub step: String,
    pub method: Method,
    pub path: String,
    pub status: Option<u16>,
    pub duration: Duration,
    pub error: Option<ErrorKind>,
    /// Sanitized, truncated; empty on success.
    pub message: String,
}

/// Values for the only supported placeholders: {{username}}, {{password}}, {{vu_id}}.
#[derive(Clone, Debug, Default)]
pub struct Vars {
    pub username: String,
    pub password: String,
    pub vu_id: u32,
}

pub fn fill(s: &str, v: &Vars) -> String {
    if !s.contains("{{") {
        return s.to_string();
    }
    s.replace("{{username}}", &v.username)
        .replace("{{password}}", &v.password)
        .replace("{{vu_id}}", &v.vu_id.to_string())
}

/// One client per virtual user: own cookie jar, own connection pool.
pub fn new_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .cookie_store(true)
        .timeout(timeout)
        .build()
        .expect("reqwest client")
}

const MAX_MESSAGE: usize = 200;

pub struct StepCtx<'a> {
    pub client: &'a reqwest::Client,
    pub base: &'a Url,
    pub default_headers: &'a [(String, String)],
    pub vars: &'a Vars,
    pub start: Instant,
}

pub async fn run_step(ctx: &StepCtx<'_>, label: &str, step: &Step) -> RequestRecord {
    let begin = Instant::now();
    let mut rec = RequestRecord {
        at: begin - ctx.start,
        vu: ctx.vars.vu_id,
        step: label.to_string(),
        method: step.method,
        path: String::new(),
        status: None,
        duration: Duration::ZERO,
        error: None,
        message: String::new(),
    };
    let url = match resolve_url(ctx.base, &fill(&step.url, ctx.vars)) {
        Ok(u) => u,
        Err(e) => {
            rec.error = Some(ErrorKind::Other);
            rec.message = truncate(format!("invalid URL: {e}"));
            return rec;
        }
    };
    rec.path = url.path().to_string();

    let method = reqwest::Method::from_bytes(step.method.as_str().as_bytes()).unwrap();
    let mut req = ctx.client.request(method, url);
    for (k, v) in ctx.default_headers.iter().chain(&step.headers) {
        req = req.header(k, fill(v, ctx.vars));
    }
    if !step.body.is_empty() {
        req = req.body(fill(&step.body, ctx.vars));
    }

    let result = async {
        let mut resp = req.send().await?;
        let status = resp.status().as_u16();
        // Drain the body so latency covers the full response without buffering it.
        while resp.chunk().await?.is_some() {}
        Ok::<u16, reqwest::Error>(status)
    }
    .await;
    rec.duration = begin.elapsed();
    match result {
        Ok(status) => {
            rec.status = Some(status);
            if !step.status_ok(status) {
                rec.error = Some(ErrorKind::Status);
                rec.message = format!("unexpected status {status}");
            }
        }
        Err(e) => {
            rec.status = e.status().map(|s| s.as_u16());
            rec.error = Some(if e.is_timeout() {
                ErrorKind::Timeout
            } else if e.is_connect() {
                ErrorKind::Connect
            } else {
                ErrorKind::Other
            });
            // without_url: the URL may carry query tokens or filled-in credentials.
            rec.message = truncate(error_chain(&e.without_url()));
        }
    }
    rec
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    s
}

fn truncate(mut s: String) -> String {
    if s.len() > MAX_MESSAGE {
        let mut end = MAX_MESSAGE;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_only_known_placeholders() {
        let v = Vars {
            username: "bob".into(),
            password: "pw".into(),
            vu_id: 7,
        };
        assert_eq!(
            fill("u={{username}}&p={{password}}&id={{vu_id}}&x={{other}}", &v),
            "u=bob&p=pw&id=7&x={{other}}"
        );
        assert_eq!(fill("plain", &v), "plain");
    }

    #[test]
    fn truncates_on_char_boundary() {
        let s = truncate("ä".repeat(150));
        assert!(s.len() <= MAX_MESSAGE + '…'.len_utf8());
        assert!(s.ends_with('…'));
    }
}
