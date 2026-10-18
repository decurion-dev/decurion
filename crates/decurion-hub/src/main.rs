use anyhow::Context;
use decurion_hub::{new_secret, router, store::Store, AppState, Keys};
use std::path::PathBuf;

/// Where the hub keeps its database and keys. Never mounted into containers.
fn data_dir() -> anyhow::Result<PathBuf> {
    if let Ok(d) = std::env::var("DECURION_DATA") {
        return Ok(PathBuf::from(d));
    }
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("HOME"))
        .context("set DECURION_DATA")?;
    Ok(PathBuf::from(base).join("decurion"))
}

/// Loads `keys.json`, creating it with fresh random keys on first start.
fn load_keys(dir: &std::path::Path) -> anyhow::Result<Keys> {
    let path = dir.join("keys.json");
    if !path.exists() {
        let v = serde_json::json!({ "owner": new_secret(), "runner": new_secret() });
        decurion_hub::write_atomic(&path, &serde_json::to_vec_pretty(&v)?)?;
    }
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    let get = |k: &str| {
        v[k].as_str()
            .map(str::to_owned)
            .with_context(|| format!("{} has no {k} key", path.display()))
    };
    Ok(Keys {
        owner: get("owner")?,
        runner: get("runner")?,
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "decurion_hub=info".into()),
        )
        .init();
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir)?;
    let keys = load_keys(&dir)?;
    let db = dir.join("hub.db");
    let store = Store::open(&format!("sqlite://{}", db.display())).await?;
    let addr = std::env::var("DECURION_ADDR").unwrap_or_else(|_| "127.0.0.1:7420".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(data = %dir.display(), "hub listening on http://{addr}");
    // Valid until the hub restarts; the owner key itself is never printed.
    let code = new_secret();
    // decurion.localhost is loopback in Chrome and Edge, and is its own site,
    // so the board's cookie is never sent to other servers on 127.0.0.1.
    let port = listener.local_addr()?.port();
    println!("Open the board: http://decurion.localhost:{port}/login?code={code}");
    let public_url = std::env::var("DECURION_PUBLIC_URL").ok();
    let pusher = decurion_hub::push::Pusher::load_or_create(&dir)?;
    let vault = decurion_vault::Vault::system(&dir);
    // Accepted work goes to GitHub with the owner's own git sign-in; the
    // token in Settings (or the GitHub CLI's sign-in) only opens the pull
    // request.
    let github = decurion_hub::github::GitHub::new(
        decurion_hub::github::GITHUB_API,
        decurion_hub::github::Tokens::Owner(Some(vault.clone())),
    )
    .context("set up sending work to GitHub")?;
    let state = AppState::new(store, keys, code, public_url)
        .with_push(pusher)
        .with_vault(vault)
        .with_files(dir.join("saved-work"))
        .with_github(github);
    // Anything accepted but not yet sent when the hub last stopped.
    let resume = state.clone();
    tokio::spawn(async move {
        let n = decurion_hub::github::resume(&resume).await;
        if n > 0 {
            tracing::info!(n, "tried again to send work that was waiting for GitHub");
        }
    });
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
