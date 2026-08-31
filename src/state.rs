//! Shared bridge state: the agent registry, task->agent mapping, and the A2A
//! client factory used to talk to registered agents.
//!
//! Populating/consuming this from MCP tools is Phase 2+; Phase 1 only wires
//! the types and the (currently unused) client factory so the dependency on
//! `a2a-client` is proven to compile end-to-end.

use std::collections::HashMap;
use std::sync::Arc;

use a2a_client::A2AClientFactory;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Metadata about a registered A2A agent, as persisted to `registered_agents.json`.
///
/// Deliberately holds no credentials: secrets are sourced from the environment
/// at startup, never written to disk here.
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

pub struct AppState {
    pub agents: RwLock<AgentRegistry>,
    pub task_agent_mapping: RwLock<TaskAgentMapping>,
    /// Shared factory for negotiating a transport (JSON-RPC/REST) from an
    /// agent's card. Individual per-agent `A2AClient`s (with auth interceptors
    /// attached) are built from this in Phase 3.
    #[allow(dead_code)]
    pub a2a_client_factory: Arc<A2AClientFactory>,
    pub data_dir: std::path::PathBuf,
}

impl AppState {
    pub fn new(data_dir: std::path::PathBuf) -> Self {
        Self {
            agents: RwLock::new(AgentRegistry::default()),
            task_agent_mapping: RwLock::new(TaskAgentMapping::default()),
            a2a_client_factory: Arc::new(A2AClientFactory::builder().build()),
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
