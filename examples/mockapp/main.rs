//! `cargo run --example mockapp [addr]` — mock web shop for trying powerwizz.
//! See `app.rs` for routes, `test.json` and `accounts.csv` for a matching test.
mod app;

#[tokio::main]
async fn main() {
    let addr = std::env::args().nth(1).unwrap_or("127.0.0.1:18080".into());
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    println!("mockapp on http://{addr} · accounts user0..user999 / pw0..pw999 · Ctrl+C to stop");
    app::spawn(listener, app::Config::default());
    std::future::pending::<()>().await;
}
