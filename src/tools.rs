//! MCP tools exposed by the bridge.
//!
//! Phase 2: agent registry (`register_agent`, `list_agents`, `unregister_agent`).
//! Phase 3 (this): real task calls to registered agents (`send_message`,
//! `get_task_result`, `cancel_task`), built on a per-agent `A2AClient` cache
//! with an optional bearer-token auth interceptor attached.
//! `send_message_stream` and non-bearer auth schemes are later phases.

use std::sync::Arc;

use a2a::{
    CancelTaskRequest, GetTaskRequest, Message, Part, Role, SendMessageRequest, SendMessageResponse,
};
use a2a_client::{agent_card::AgentCardResolver, auth::AuthInterceptor};
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::json;

use crate::persistence;
use crate::state::{AgentInfo, AppState, SharedA2AClient};

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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RegisterAgentRequest {
    /// URL of the A2A agent
    url: String,
    /// Bearer token to authenticate outbound calls to this agent, if it
    /// requires one. Held in memory only -- never written to disk.
    #[serde(default)]
    bearer_token: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SendMessageArgs {
    /// URL of the registered A2A agent to send the message to
    agent_url: String,
    /// Message text to send
    message: String,
    /// Optional context ID to continue an existing multi-turn conversation
    #[serde(default)]
    context_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TaskIdArgs {
    /// ID of the task, as returned by send_message
    task_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetTaskResultArgs {
    /// ID of the task, as returned by send_message
    task_id: String,
    /// Optional number of history messages to include
    #[serde(default)]
    history_length: Option<i32>,
}

fn text_result(body: serde_json::Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(
        body.to_string(),
    )]))
}

fn error_result(message: impl Into<String>) -> Result<CallToolResult, McpError> {
    text_result(json!({ "status": "error", "message": message.into() }))
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
                        A2A-v1-compatible server (inbound). Pass bearer_token if the agent \
                        requires bearer authentication."
    )]
    async fn register_agent(
        &self,
        Parameters(RegisterAgentRequest { url, bearer_token }): Parameters<RegisterAgentRequest>,
    ) -> Result<CallToolResult, McpError> {
        let resolver = AgentCardResolver::new(None);
        let card = match resolver.resolve(&url).await {
            Ok(card) => card,
            Err(e) => {
                return error_result(format!("Failed to fetch agent card from {url}: {e}"));
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
        if let Some(token) = &bearer_token {
            self.state
                .credentials
                .write()
                .await
                .insert(url.clone(), token.clone());
        }
        // Card and/or credentials may have changed; force a rebuild on next use.
        self.state.agent_clients.write().await.remove(&url);
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
            "bearer_token_set": bearer_token.is_some(),
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
            return error_result(format!("Agent not registered: {url}"));
        };

        self.state.credentials.write().await.remove(&url);
        self.state.agent_clients.write().await.remove(&url);

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

    #[tool(description = "Send a message to a registered A2A agent")]
    async fn send_message(
        &self,
        Parameters(SendMessageArgs {
            agent_url,
            message,
            context_id,
        }): Parameters<SendMessageArgs>,
    ) -> Result<CallToolResult, McpError> {
        if !self.state.agents.read().await.0.contains_key(&agent_url) {
            return error_result(format!("Agent not registered: {agent_url}"));
        }

        let client = match self.client_for(&agent_url).await {
            Ok(client) => client,
            Err(e) => return error_result(e),
        };

        let mut a2a_message = Message::new(Role::User, vec![Part::text(message)]);
        a2a_message.context_id = context_id;

        let req = SendMessageRequest {
            message: a2a_message,
            configuration: None,
            metadata: None,
            tenant: None,
        };

        match client.send_message(&req).await {
            Ok(SendMessageResponse::Task(task)) => {
                {
                    let mut mapping = self.state.task_agent_mapping.write().await;
                    mapping.0.insert(task.id.clone(), agent_url.clone());
                }
                self.persist_task_mapping().await;
                text_result(json!({ "status": "success", "task": task }))
            }
            Ok(SendMessageResponse::Message(reply)) => text_result(json!({
                "status": "success",
                "message": reply,
            })),
            Err(e) => error_result(format!("Error sending message: {e}")),
        }
    }

    #[tool(description = "Retrieve the result of a task from an A2A agent")]
    async fn get_task_result(
        &self,
        Parameters(GetTaskResultArgs {
            task_id,
            history_length,
        }): Parameters<GetTaskResultArgs>,
    ) -> Result<CallToolResult, McpError> {
        let Some(agent_url) = self
            .state
            .task_agent_mapping
            .read()
            .await
            .0
            .get(&task_id)
            .cloned()
        else {
            return error_result(format!("Task ID not found: {task_id}"));
        };

        let client = match self.client_for(&agent_url).await {
            Ok(client) => client,
            Err(e) => return error_result(e),
        };

        let req = GetTaskRequest {
            id: task_id.clone(),
            history_length,
            tenant: None,
        };

        match client.get_task(&req).await {
            Ok(task) => text_result(json!({ "status": "success", "task": task })),
            Err(e) => error_result(format!("Error retrieving task result: {e}")),
        }
    }

    #[tool(description = "Cancel a running task on an A2A agent")]
    async fn cancel_task(
        &self,
        Parameters(TaskIdArgs { task_id }): Parameters<TaskIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let Some(agent_url) = self
            .state
            .task_agent_mapping
            .read()
            .await
            .0
            .get(&task_id)
            .cloned()
        else {
            return error_result(format!("Task ID not found: {task_id}"));
        };

        let client = match self.client_for(&agent_url).await {
            Ok(client) => client,
            Err(e) => return error_result(e),
        };

        let req = CancelTaskRequest {
            id: task_id.clone(),
            metadata: None,
            tenant: None,
        };

        match client.cancel_task(&req).await {
            Ok(task) => text_result(json!({ "status": "success", "task": task })),
            Err(e) => error_result(format!("Error cancelling task: {e}")),
        }
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

    /// Returns a cached per-agent client, building (and caching) one on first
    /// use: resolves the agent's card, negotiates a transport via the shared
    /// factory, and attaches a bearer-auth interceptor if a credential is on
    /// file for this agent.
    async fn client_for(&self, agent_url: &str) -> Result<SharedA2AClient, String> {
        if let Some(client) = self.state.agent_clients.read().await.get(agent_url) {
            return Ok(client.clone());
        }

        let resolver = AgentCardResolver::new(None);
        let card = resolver
            .resolve(agent_url)
            .await
            .map_err(|e| format!("failed to fetch agent card: {e}"))?;

        let mut client = self
            .state
            .a2a_client_factory
            .create_from_card(&card)
            .await
            .map_err(|e| format!("failed to negotiate a transport with the agent: {e}"))?;

        if let Some(token) = self.state.credentials.read().await.get(agent_url) {
            client =
                client.with_interceptors(vec![Arc::new(AuthInterceptor::bearer(token.clone()))]);
        }

        let client = Arc::new(client);
        self.state
            .agent_clients
            .write()
            .await
            .insert(agent_url.to_string(), client.clone());
        Ok(client)
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
                 already run its own A2A server), then send it messages with send_message and \
                 retrieve results with get_task_result. Streaming responses land in a later \
                 phase."
                    .to_string(),
            )
    }
}
