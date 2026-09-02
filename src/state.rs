//! Shared bridge state: the agent registry, task->agent mapping, per-agent
//! A2A clients (with auth interceptors attached), and the credentials that
//! back them.

use std::collections::HashMap;
use std::sync::Arc;

use a2a_client::{A2AClient, A2AClientFactory, Transport};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Metadata about a registered A2A agent, as persisted to `registered_agents.json`.
///
/// Deliberately holds no credentials: secrets live only in `AppState::credentials`,
/// which is never persisted to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub url: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AgentRegistry(pub HashMap<String, AgentInfo>);

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TaskAgentMapping(pub HashMap<String, String>);

/// A negotiated, possibly-authenticated client for one agent.
pub type SharedA2AClient = Arc<A2AClient<Box<dyn Transport>>>;

pub struct AppState {
    pub agents: RwLock<AgentRegistry>,
    pub task_agent_mapping: RwLock<TaskAgentMapping>,
    /// Shared factory for negotiating a transport (JSON-RPC/REST) from an
    /// agent's card.
    pub a2a_client_factory: Arc<A2AClientFactory>,
    /// Bearer tokens per agent URL, supplied via `register_agent`'s optional
    /// `bearer_token` argument. In-memory only -- never written to disk.
    pub credentials: RwLock<HashMap<String, String>>,
    /// Per-agent client cache, built lazily on first use and evicted whenever
    /// `register_agent`/`unregister_agent` touch that URL (card or
    /// credentials may have changed).
    pub agent_clients: RwLock<HashMap<String, SharedA2AClient>>,
    pub data_dir: std::path::PathBuf,
}

impl AppState {
    pub fn new(data_dir: std::path::PathBuf) -> Self {
        Self {
            agents: RwLock::new(AgentRegistry::default()),
            task_agent_mapping: RwLock::new(TaskAgentMapping::default()),
            a2a_client_factory: Arc::new(A2AClientFactory::builder().build()),
            credentials: RwLock::new(HashMap::new()),
            agent_clients: RwLock::new(HashMap::new()),
            data_dir,
        }
    }

    pub fn registered_agents_path(&self) -> std::path::PathBuf {
        self.data_dir.join("registered_agents.json")
    }

    pub fn task_agent_mapping_path(&self) -> std::path::PathBuf {
        self.data_dir.join("task_agent_mapping.json")
    }
}
