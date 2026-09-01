mod persistence;
mod state;
mod tools;

use std::sync::Arc;
use std::time::Duration;

use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

use state::AppState;
use tools::Bridge;

const DEFAULT_TRANSPORT: &str = "stdio";
const PERIODIC_SAVE_INTERVAL: Duration = Duration::from_secs(300);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();

    let data_dir = std::env::var("A2A_MCP_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut state = AppState::new(data_dir);
    load_state(&mut state);
    let state = Arc::new(state);

    // Only used to reach persist_agents/persist_task_mapping from here; the
    // transports below build their own Bridge(s) sharing the same AppState.
    let janitor = Bridge::new(state.clone());
    tokio::spawn(periodic_save(janitor.clone()));

    let transport = std::env::var("MCP_TRANSPORT").unwrap_or_else(|_| DEFAULT_TRANSPORT.into());

    let result = match transport.as_str() {
        "stdio" => run_stdio(state.clone()).await,
        "streamable-http" => run_streamable_http(state.clone()).await,
        other => {
            anyhow::bail!("unknown MCP_TRANSPORT '{other}', expected 'stdio' or 'streamable-http'");
        }
    };

    tracing::info!("saving state before exit");
    janitor.persist_agents().await;
    janitor.persist_task_mapping().await;
    result
}

fn load_state(state: &mut AppState) {
    let agents: state::AgentRegistry = persistence::load_from_json(state.registered_agents_path());
    let mapping: state::TaskAgentMapping =
        persistence::load_from_json(state.task_agent_mapping_path());
    tracing::info!(
        agents = agents.0.len(),
        tasks = mapping.0.len(),
        "loaded persisted state"
    );
    state.agents = tokio::sync::RwLock::new(agents);
    state.task_agent_mapping = tokio::sync::RwLock::new(mapping);
}

/// Defensive redundancy: every registry mutation already persists immediately
/// (see `tools::Bridge`), plus a save-on-exit in `main`. This just covers the
/// gap between "still running" and either of those.
async fn periodic_save(bridge: Bridge) {
    let mut interval = tokio::time::interval(PERIODIC_SAVE_INTERVAL);
    interval.tick().await; // first tick fires immediately; skip it
    loop {
        interval.tick().await;
        tracing::info!("performing periodic state save");
        bridge.persist_agents().await;
        bridge.persist_task_mapping().await;
    }
}

async fn run_stdio(state: Arc<AppState>) -> anyhow::Result<()> {
    tracing::info!("starting MCP server on stdio transport");
    let service = Bridge::new(state).serve(stdio()).await.inspect_err(|e| {
        tracing::error!(error = %e, "failed to start stdio transport");
    })?;
    service.waiting().await?;
    Ok(())
}

async fn run_streamable_http(state: Arc<AppState>) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    let host = std::env::var("MCP_HOST").unwrap_or_else(|_| "0.0.0.0".into());
    let port: u16 = std::env::var("MCP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8000);
    let path = std::env::var("MCP_PATH").unwrap_or_else(|_| "/mcp".into());
    let bind_address = format!("{host}:{port}");

    let ct = tokio_util::sync::CancellationToken::new();
    let service = StreamableHttpService::new(
        move || Ok(Bridge::new(state.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
    );

    let router = axum::Router::new().nest_service(&path, service);
    let listener = tokio::net::TcpListener::bind(&bind_address).await?;
    tracing::info!(address = %bind_address, path = %path, "starting MCP server on streamable-http transport");

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            ct.cancel();
        })
        .await?;
    Ok(())
}
