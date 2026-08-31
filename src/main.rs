mod persistence;
mod state;

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use tracing_subscriber::EnvFilter;

use state::AppState;

const DEFAULT_TRANSPORT: &str = "stdio";

#[derive(Clone)]
struct Bridge {
    state: Arc<AppState>,
    // Read by the `#[tool_handler]`-generated `call_tool`/`list_tools` dispatch;
    // rustc's dead-code pass doesn't see through that macro expansion.
    #[allow(dead_code)]
    tool_router: ToolRouter<Bridge>,
}

#[tool_router]
impl Bridge {
    fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    /// Placeholder health-check tool for Phase 1. Registry/A2A tools
    /// (register_agent, send_message, ...) land in Phase 2+.
    #[tool(description = "Report bridge status: version and number of registered agents")]
    async fn status(&self) -> Result<CallToolResult, McpError> {
        let agent_count = self.state.agents.read().await.0.len();
        let body = serde_json::json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "registered_agents": agent_count,
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            body.to_string(),
        )]))
    }
}

#[tool_handler]
impl ServerHandler for Bridge {
    fn get_info(&self) -> ServerInfo {
        // NB: `Implementation::from_build_env()` reads `env!("CARGO_CRATE_NAME")` at
        // *rmcp's* compile time, so it always reports "rmcp" rather than this crate.
        // Set it explicitly instead.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "MCP bridge to A2A agents. Register agents, then send them messages and \
                 retrieve task results. See README for the full tool list as it lands."
                    .to_string(),
            )
    }
}

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

    let transport = std::env::var("MCP_TRANSPORT").unwrap_or_else(|_| DEFAULT_TRANSPORT.into());

    match transport.as_str() {
        "stdio" => run_stdio(state).await,
        "streamable-http" => run_streamable_http(state).await,
        other => {
            anyhow::bail!("unknown MCP_TRANSPORT '{other}', expected 'stdio' or 'streamable-http'");
        }
    }
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
