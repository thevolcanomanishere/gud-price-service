use gud_price_service::{AppState, Config, GudPriceProvider, app};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let config = Config::from_env();

    let state = AppState::new(config.cache_ttl, Arc::new(GudPriceProvider::new()));
    let app = app(state);

    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .expect("failed to bind TCP listener");

    println!(
        "gud-price-service listening on http://{} (cache TTL: {}s)",
        config.bind_addr,
        config.cache_ttl_secs()
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server failed");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C signal handler");
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        sigterm.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
