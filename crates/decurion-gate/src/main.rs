//! The gate binary. Reads its list from `DECURION_SITES`, listens on port
//! 3128 and prints `{"event":"ready"}` once it can take connections. With
//! `DECURION_RELAY` (`host:port` of the gatekeeper), it also passes port
//! 3129 straight through to that one address.

use anyhow::Context;
use decurion_gate::{print_event, relay, serve, Event, Gate, Policy, PORT, RELAY_PORT};
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // No list means nothing gets through, which is the safe way to be wrong.
    let list = std::env::var("DECURION_SITES").unwrap_or_default();
    let (policy, rejected) = Policy::from_list(&list);
    for entry in rejected {
        eprintln!("decurion-gate: ignoring {entry:?}: not a website name");
    }

    let listener = TcpListener::bind(("0.0.0.0", PORT))
        .await
        .with_context(|| format!("listening on port {PORT}"))?;
    let relayed = match std::env::var("DECURION_RELAY")
        .ok()
        .filter(|t| !t.is_empty())
    {
        Some(target) => {
            let l = TcpListener::bind(("0.0.0.0", RELAY_PORT))
                .await
                .with_context(|| format!("listening on port {RELAY_PORT}"))?;
            Some(tokio::spawn(relay(l, target)))
        }
        None => None,
    };
    let gate = Arc::new(Gate::new(policy, print_event));
    print_event(Event::Ready);

    // The gate is process 1 in its container, which gets no default signal
    // handling, so it has to stop on `docker stop` itself.
    tokio::select! {
        served = serve(listener, gate) => served.context("serving")?,
        _ = stop_signal() => {}
    }
    if let Some(r) = relayed {
        r.abort();
    }
    Ok(())
}

#[cfg(unix)]
async fn stop_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

#[cfg(not(unix))]
async fn stop_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
