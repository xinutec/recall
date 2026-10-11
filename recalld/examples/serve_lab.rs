//! The web app over a database copy, signed in, on localhost: to look at what a
//! change to the reads shows before it ships. An example, so nothing ships.
//!
//! Prints the session cookie to set for `http://127.0.0.1:PORT` (name=value).
//!
//! Usage: `cargo run --release --example serve_lab -- --root LAB --frontend DIST --port 18001`

use recalld::app::{Config, DEFAULT_MAX_BODY, router};
use recalld::webauth::{self, COOKIE_NAME, GateState};
use std::path::PathBuf;
use std::sync::Arc;

const SECRET: &str = "serve-lab-not-a-real-secret";

#[derive(clap::Parser)]
struct Args {
    /// The copy's data root.
    #[arg(long)]
    root: PathBuf,
    /// The built web app.
    #[arg(long)]
    frontend: PathBuf,
    #[arg(long, default_value_t = 18001)]
    port: u16,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = <Args as clap::Parser>::parse();
    let now = chrono::Utc::now().timestamp();
    let gate = GateState {
        cfg: Arc::new(webauth::Config {
            session_secret: SECRET.into(),
            client_id: "lab".into(),
            client_secret: "lab".into(),
            nc_base_url: "https://nextcloud.invalid".into(),
            nc_internal_url: "https://nextcloud.invalid".into(),
            redirect_uri: "https://recall.invalid/auth/callback".into(),
            allowed_users: std::collections::HashSet::new(),
            device_token: None,
        }),
        now: Arc::new(move || now),
    };
    let cookie = webauth::make_session_cookie(
        SECRET,
        &webauth::Session {
            user_id: "lab".into(),
            display_name: "lab".into(),
        },
        now,
    )
    .ok_or("cannot sign a session")?;
    let app = router(Arc::new(Config {
        root: args.root,
        tokens: None,
        read_token: None,
        max_body_bytes: DEFAULT_MAX_BODY,
        trusted_proxies: Vec::new(),
        webauth: Some(gate),
        sync_token: None,
        frontend: Some(args.frontend),
    }));
    println!("{COOKIE_NAME}={cookie}");
    tokio::runtime::Runtime::new()?.block_on(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", args.port)).await?;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    })?;
    Ok(())
}
