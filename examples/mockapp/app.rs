//! Mock web shop as a realistic load-test target. In-memory, HTTP/1.1 with keep-alive.
//!
//! Accounts: `user0`…`user999`, password `pw<N>`.
//!
//! | Route | Behaviour |
//! |---|---|
//! | `GET /health` | 200, no session needed |
//! | `POST /api/login` | JSON `{"username","password"}` → 200 + `sid` cookie, 401 on bad credentials |
//! | `POST /api/logout` | 204, ends the session |
//! | `GET /api/items` | first 20 items |
//! | `GET /api/items/{id}` | one item, 404 for id ≥ 1000 |
//! | `POST /api/orders` | JSON `{"item","qty"}` → 201, 400 on bad input, 500 at `error_rate` |
//! | `GET /api/slow` | like items, but `slow_ms` latency |
//! | `GET /login` | HTML form with a CSRF token (cookie + hidden field) |
//! | `POST /login` | form login; 403 without the matching CSRF token, else 303 + `sid` |
//!
//! `/api/*` except login needs a session (401), is rate-limited per session (429)
//! and answers after `latency_ms`.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

pub const ITEMS: u64 = 1000;
pub const ACCOUNTS: u64 = 1000;

#[derive(Clone, Debug)]
pub struct Config {
    /// Min/max latency of API responses.
    pub latency_ms: (u64, u64),
    pub slow_ms: (u64, u64),
    /// Share of orders answered with 500.
    pub error_rate: f64,
    /// Requests per session and second; 0 = unlimited.
    pub rate_limit: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            latency_ms: (5, 40),
            slow_ms: (300, 800),
            error_rate: 0.01,
            rate_limit: 20,
        }
    }
}

#[derive(Default, Debug)]
pub struct Stats {
    pub requests: AtomicU64,
    pub logins: AtomicU64,
    pub orders: AtomicU64,
}

struct Session {
    window: u64,
    count: u32,
}

struct State {
    cfg: Config,
    start: Instant,
    sessions: Mutex<HashMap<String, Session>>,
    csrf: Mutex<HashSet<String>>,
    stats: Arc<Stats>,
}

struct Req {
    method: String,
    path: String,
    cookie: String,
    body: String,
}

struct Resp {
    status: u16,
    headers: String,
    content_type: &'static str,
    body: String,
}

fn resp(status: u16, body: Value) -> Resp {
    Resp {
        status,
        headers: String::new(),
        content_type: "application/json",
        body: body.to_string(),
    }
}

/// Serves on `listener` until the runtime shuts down.
pub fn spawn(listener: TcpListener, cfg: Config) -> Arc<Stats> {
    let stats = Arc::new(Stats::default());
    let st = Arc::new(State {
        cfg,
        start: Instant::now(),
        sessions: Mutex::default(),
        csrf: Mutex::default(),
        stats: stats.clone(),
    });
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let st = st.clone();
            tokio::spawn(async move {
                let mut sock = BufReader::new(sock);
                while let Some(req) = read_request(&mut sock).await {
                    st.stats.requests.fetch_add(1, Ordering::Relaxed);
                    let r = handle(&st, req).await;
                    let out = format!(
                        "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\n{}\r\n{}",
                        r.status,
                        r.content_type,
                        r.body.len(),
                        r.headers,
                        r.body
                    );
                    if sock.get_mut().write_all(out.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    stats
}

async fn read_request(sock: &mut BufReader<tokio::net::TcpStream>) -> Option<Req> {
    let mut line = String::new();
    if sock.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.split('?').next()?.to_string();
    let (mut len, mut cookie) = (0usize, String::new());
    loop {
        let mut h = String::new();
        if sock.read_line(&mut h).await.ok()? == 0 {
            return None;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':').unwrap_or((h, ""));
        match k.to_ascii_lowercase().as_str() {
            "content-length" => len = v.trim().parse().ok()?,
            "cookie" => cookie = v.trim().to_string(),
            _ => {}
        }
    }
    let mut body = vec![0; len];
    sock.read_exact(&mut body).await.ok()?;
    Some(Req {
        method,
        path,
        cookie,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

// ponytail: no percent-decoding; mock credentials and tokens are plain ASCII.
fn form_field<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    body.split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn valid_login(user: &str, pw: &str) -> bool {
    user.strip_prefix("user")
        .and_then(|n| n.parse::<u64>().ok())
        .is_some_and(|n| n < ACCOUNTS && pw == format!("pw{n}"))
}

fn token() -> String {
    format!("{:016x}{:016x}", fastrand::u64(..), fastrand::u64(..))
}

async fn sleep_between((lo, hi): (u64, u64)) {
    tokio::time::sleep(Duration::from_millis(fastrand::u64(lo..=hi.max(lo)))).await;
}

fn item(id: u64) -> Value {
    json!({ "id": id, "name": format!("Artikel {id}"), "price": 100 + id % 900, "stock": id * 7 % 50 })
}

fn new_session(st: &State) -> String {
    let sid = token();
    st.sessions.lock().unwrap().insert(
        sid.clone(),
        Session {
            window: 0,
            count: 0,
        },
    );
    st.stats.logins.fetch_add(1, Ordering::Relaxed);
    sid
}

async fn handle(st: &State, req: Req) -> Resp {
    let segs: Vec<&str> = req.path.trim_matches('/').split('/').collect();
    match (req.method.as_str(), segs.as_slice()) {
        ("GET", ["health"]) => resp(200, json!("ok")),
        ("POST", ["api", "login"]) => {
            let v: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let (u, p) = (
                v["username"].as_str().unwrap_or(""),
                v["password"].as_str().unwrap_or(""),
            );
            if !valid_login(u, p) {
                return resp(401, json!({ "error": "invalid credentials" }));
            }
            let mut r = resp(200, json!({ "user": u }));
            r.headers = format!("Set-Cookie: sid={}; Path=/; HttpOnly\r\n", new_session(st));
            r
        }
        ("GET", ["login"]) => {
            let t = token();
            st.csrf.lock().unwrap().insert(t.clone());
            Resp {
                status: 200,
                headers: format!("Set-Cookie: csrf={t}; Path=/\r\n"),
                content_type: "text/html",
                body: format!(
                    "<form method=post action=/login><input type=hidden name=csrf value={t}>\
                     <input name=username><input name=password type=password><button>Login</button></form>"
                ),
            }
        }
        ("POST", ["login"]) => {
            let sent = form_field(&req.body, "csrf");
            let ok = sent.is_some()
                && sent == cookie(&req.cookie, "csrf")
                && st.csrf.lock().unwrap().remove(sent.unwrap());
            if !ok {
                return resp(403, json!({ "error": "csrf token missing or invalid" }));
            }
            let (u, p) = (
                form_field(&req.body, "username").unwrap_or(""),
                form_field(&req.body, "password").unwrap_or(""),
            );
            if !valid_login(u, p) {
                return resp(401, json!({ "error": "invalid credentials" }));
            }
            let mut r = resp(303, json!({}));
            r.headers = format!(
                "Location: /\r\nSet-Cookie: sid={}; Path=/; HttpOnly\r\n",
                new_session(st)
            );
            r
        }
        (_, ["api", ..]) => api(st, &req, &segs[1..]).await,
        _ => resp(404, json!({ "error": "not found" })),
    }
}

async fn api(st: &State, req: &Req, segs: &[&str]) -> Resp {
    let Some(sid) = cookie(&req.cookie, "sid").map(str::to_owned) else {
        return resp(401, json!({ "error": "no session" }));
    };
    {
        let mut sessions = st.sessions.lock().unwrap();
        let Some(s) = sessions.get_mut(&sid) else {
            return resp(401, json!({ "error": "session expired" }));
        };
        let now = st.start.elapsed().as_secs();
        if s.window != now {
            (s.window, s.count) = (now, 0);
        }
        s.count += 1;
        if st.cfg.rate_limit > 0 && s.count > st.cfg.rate_limit {
            return resp(429, json!({ "error": "rate limit" }));
        }
    }
    match (req.method.as_str(), segs) {
        ("POST", ["logout"]) => {
            st.sessions.lock().unwrap().remove(&sid);
            resp(204, json!(null))
        }
        ("GET", ["items"]) => {
            sleep_between(st.cfg.latency_ms).await;
            resp(200, json!((0..20).map(item).collect::<Vec<_>>()))
        }
        ("GET", ["items", id]) => {
            sleep_between(st.cfg.latency_ms).await;
            match id.parse::<u64>() {
                Ok(id) if id < ITEMS => resp(200, item(id)),
                _ => resp(404, json!({ "error": "no such item" })),
            }
        }
        ("POST", ["orders"]) => {
            sleep_between((st.cfg.latency_ms.0 * 2, st.cfg.latency_ms.1 * 2)).await;
            let v: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let (Some(item), Some(qty)) = (v["item"].as_u64(), v["qty"].as_u64()) else {
                return resp(400, json!({ "error": "item and qty required" }));
            };
            if item >= ITEMS || qty == 0 {
                return resp(400, json!({ "error": "invalid order" }));
            }
            if fastrand::f64() < st.cfg.error_rate {
                return resp(500, json!({ "error": "internal error" }));
            }
            let n = st.stats.orders.fetch_add(1, Ordering::Relaxed) + 1;
            resp(201, json!({ "order": n, "item": item, "qty": qty }))
        }
        ("GET", ["slow"]) => {
            sleep_between(st.cfg.slow_ms).await;
            resp(200, json!((0..20).map(item).collect::<Vec<_>>()))
        }
        _ => resp(404, json!({ "error": "not found" })),
    }
}
