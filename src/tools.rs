//! MCP tools exposed by the bridge.
//!
//! Phase 2 scope: agent registry (`register_agent`, `list_agents`,
//! `unregister_agent`), backed by the state/persistence introduced in
//! Phase 1. `register_agent` resolves the agent's real card via
//! `a2a-client`'s `AgentCardResolver` rather than trusting the URL blindly.
//! Sending messages/tasks to registered agents lands in Phase 3.

use std::sync::Arc;

use a2a_client::agent_card::AgentCardResolver;
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::json;

use crate::persistence;
use crate::state::{AgentInfo, AppState};

#[derive(Clone)]
pub struct Bridge {
    state: Arc<AppState>,
    // Read by the `#[tool_handler]`-generated `call_tool`/`list_tools` dispatch;
    // rustc's dead-code pass doesn't see through that macro expansion.
    #[allow(dead_code)]
    tool_router: ToolRouter<Bridge>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AgentUrlRequest {
    /// URL of the A2A agent
    url: String,
}

fn text_result(body: serde_json::Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(
        body.to_string(),
    )]))
}

#[tool_router]
impl Bridge {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Report bridge status: version and number of registered agents")]
    async fn status(&self) -> Result<CallToolResult, McpError> {
        let agent_count = self.state.agents.read().await.0.len();
        text_result(json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "registered_agents": agent_count,
        }))
    }

    #[tool(
        description = "Register an A2A agent with the bridge by fetching its agent card from \
                        {url}/.well-known/agent-card.json. The agent must already run its own \
                        A2A-v1-compatible server (inbound)."
    )]
    async fn register_agent(
        &self,
        Parameters(AgentUrlRequest { url }): Parameters<AgentUrlRequest>,
    ) -> Result<CallToolResult, McpError> {
        let resolver = AgentCardResolver::new(None);
        let card = match resolver.resolve(&url).await {
            Ok(card) => card,
            Err(e) => {
                return text_result(json!({
                    "status": "error",
                    "message": format!("Failed to fetch agent card from {url}: {e}"),
                }));
            }
        };

        let info = AgentInfo {
            url: url.clone(),
            name: card.name.clone(),
            description: if card.description.is_empty() {
                "No description provided".to_string()
            } else {
                card.description.clone()
            },
        };

        {
            let mut agents = self.state.agents.write().await;
            agents.0.insert(url.clone(), info.clone());
        }
        self.persist_agents().await;

        // Surface which auth scheme(s) the agent declares, so misconfiguration
        // is caught here rather than as a 401 deep inside send_message later.
        let requires_auth: Vec<&String> = card
            .security_schemes
            .as_ref()
            .map(|schemes| schemes.keys().collect())
            .unwrap_or_default();

        text_result(json!({
            "status": "success",
            "agent": info,
            "requires_auth": requires_auth,
        }))
    }

    #[tool(description = "List all registered A2A agents")]
    async fn list_agents(&self) -> Result<CallToolResult, McpError> {
        let agents = self.state.agents.read().await;
        let list: Vec<&AgentInfo> = agents.0.values().collect();
        text_result(json!(list))
    }

    #[tool(description = "Unregister an A2A agent from the bridge")]
    async fn unregister_agent(
        &self,
        Parameters(AgentUrlRequest { url }): Parameters<AgentUrlRequest>,
    ) -> Result<CallToolResult, McpError> {
        let removed = {
            let mut agents = self.state.agents.write().await;
            agents.0.remove(&url)
        };

        let Some(removed) = removed else {
            return text_result(json!({
                "status": "error",
                "message": format!("Agent not registered: {url}"),
            }));
        };

        let removed_tasks = {
            let mut mapping = self.state.task_agent_mapping.write().await;
            let to_remove: Vec<String> = mapping
                .0
                .iter()
                .filter(|(_, agent_url)| **agent_url == url)
                .map(|(task_id, _)| task_id.clone())
                .collect();
            for task_id in &to_remove {
                mapping.0.remove(task_id);
            }
            to_remove.len()
        };

        self.persist_agents().await;
        self.persist_task_mapping().await;

        text_result(json!({
            "status": "success",
            "message": format!("Successfully unregistered agent: {}", removed.name),
            "removed_tasks": removed_tasks,
        }))
    }
}

impl Bridge {
    pub async fn persist_agents(&self) {
        let agents = self.state.agents.read().await;
        if let Err(e) = persistence::save_to_json(&*agents, self.state.registered_agents_path()) {
            tracing::warn!(error = %e, "failed to persist registered agents");
        }
    }

    pub async fn persist_task_mapping(&self) {
        let mapping = self.state.task_agent_mapping.read().await;
        if let Err(e) = persistence::save_to_json(&*mapping, self.state.task_agent_mapping_path()) {
            tracing::warn!(error = %e, "failed to persist task mapping");
        }
    }
}

#[tool_handler]
impl ServerHandler for Bridge {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "MCP bridge to A2A agents. Register an agent with register_agent (it must \
                 already run its own A2A server), then list registered agents with list_agents. \
                 Sending messages/tasks to agents lands in a later phase."
                    .to_string(),
            )
    }
}
