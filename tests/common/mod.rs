//! Minimal HTTP/1.1 test server on a random local port. No extra dependencies.
//!
//! Routes:
//! - `/status/{code}`          respond with that status
//! - `/delay/{ms}`             sleep, then 200
//! - `/set-cookie/{v}`         200 with `Set-Cookie: sid={v}`
//! - `/cookie/{v}`             200 if the request carries `sid={v}`, else 403
//! - `/login`                  200 + `Set-Cookie: sid={body}` if the body is non-empty, else 401
//! - `/drop`                   close the connection without responding
//! - anything else             200
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

pub struct TestServer {
    pub addr: SocketAddr,
    pub hits: Arc<AtomicU64>,
    /// (time since server start, request path) of every request.
    pub log: Arc<Mutex<Vec<(Duration, String)>>>,
    /// Bodies received on /login.
    pub bodies: Arc<Mutex<Vec<String>>>,
}

impl TestServer {
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }
}

pub async fn start() -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let srv = TestServer {
        addr: listener.local_addr().unwrap(),
        hits: Arc::default(),
        log: Arc::default(),
        bodies: Arc::default(),
    };
    let (hits, log, bodies) = (srv.hits.clone(), srv.log.clone(), srv.bodies.clone());
    let t0 = Instant::now();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            let (hits, log, bodies) = (hits.clone(), log.clone(), bodies.clone());
            tokio::spawn(async move {
                let mut sock = BufReader::new(sock);
                loop {
                    let mut line = String::new();
                    if sock.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let (mut len, mut cookie) = (0usize, String::new());
                    loop {
                        let mut h = String::new();
                        if sock.read_line(&mut h).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let h = h.trim_end();
                        if h.is_empty() {
                            break;
                        }
                        let (k, v) = h.split_once(':').unwrap_or((h, ""));
                        match k.to_ascii_lowercase().as_str() {
                            "content-length" => len = v.trim().parse().unwrap_or(0),
                            "cookie" => cookie = v.trim().to_string(),
                            _ => {}
                        }
                    }
                    let mut body = vec![0; len];
                    if sock.read_exact(&mut body).await.is_err() {
                        return;
                    }
                    let body = String::from_utf8_lossy(&body).into_owned();
                    hits.fetch_add(1, Ordering::Relaxed);
                    log.lock().unwrap().push((t0.elapsed(), path.clone()));

                    let mut status = 200;
                    let mut extra = String::new();
                    let route = path.split('?').next().unwrap_or("");
                    let mut parts = route.trim_start_matches('/').splitn(2, '/');
                    match (parts.next().unwrap_or(""), parts.next().unwrap_or("")) {
                        ("status", c) => status = c.parse().unwrap_or(500),
                        ("delay", ms) => {
                            tokio::time::sleep(Duration::from_millis(ms.parse().unwrap_or(0))).await
                        }
                        ("set-cookie", v) => extra = format!("Set-Cookie: sid={v}; Path=/\r\n"),
                        ("cookie", v) => {
                            if !cookie.split("; ").any(|c| c == format!("sid={v}")) {
                                status = 403;
                            }
                        }
                        ("login", _) => {
                            if body.is_empty() {
                                status = 401;
                            } else {
                                extra = format!("Set-Cookie: sid={body}; Path=/\r\n");
                                bodies.lock().unwrap().push(body);
                            }
                        }
                        ("drop", _) => return,
                        _ => {}
                    }
                    let resp = format!("HTTP/1.1 {status} X\r\n{extra}Content-Length: 2\r\n\r\nok");
                    if sock.get_mut().write_all(resp.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    srv
}
