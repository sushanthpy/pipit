//! Node identity, descriptor, and mesh daemon lifecycle.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Unique node identifier (UUID v4).
pub type NodeId = String;

/// Descriptor broadcast by each node in the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDescriptor {
    pub id: NodeId,
    pub name: String,
    pub addr: SocketAddr,
    pub capabilities: Vec<String>,
    pub model: Option<String>,
    pub load: f64,
    pub gpu: Option<GpuInfo>,
    pub project_roots: Vec<String>,
    pub joined_at: chrono::DateTime<chrono::Utc>,
    pub last_heartbeat: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuInfo {
    pub name: String,
    pub count: u32,
    pub vram_gb: f64,
}

/// Health status of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    Alive,
    Suspect,
    Dead,
}

/// The node registry — all known nodes in the mesh.
#[derive(Debug)]
pub struct NodeRegistry {
    nodes: HashMap<NodeId, (NodeDescriptor, NodeStatus)>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    pub fn upsert(&mut self, desc: NodeDescriptor) {
        self.nodes
            .insert(desc.id.clone(), (desc, NodeStatus::Alive));
    }

    pub fn mark_suspect(&mut self, id: &str) {
        if let Some((_, status)) = self.nodes.get_mut(id) {
            *status = NodeStatus::Suspect;
        }
    }

    pub fn mark_dead(&mut self, id: &str) {
        if let Some((_, status)) = self.nodes.get_mut(id) {
            *status = NodeStatus::Dead;
        }
    }

    pub fn evict_dead(&mut self) {
        self.nodes
            .retain(|_, (_, status)| *status != NodeStatus::Dead);
    }

    pub fn alive_nodes(&self) -> Vec<&NodeDescriptor> {
        self.nodes
            .values()
            .filter(|(_, status)| *status == NodeStatus::Alive)
            .map(|(desc, _)| desc)
            .collect()
    }

    pub fn all_nodes(&self) -> Vec<(&NodeDescriptor, &NodeStatus)> {
        self.nodes.values().map(|(d, s)| (d, s)).collect()
    }

    /// Find nodes matching a capability query.
    pub fn find_by_capability(&self, required: &[String]) -> Vec<&NodeDescriptor> {
        self.alive_nodes()
            .into_iter()
            .filter(|node| required.iter().all(|cap| node.capabilities.contains(cap)))
            .collect()
    }

    /// Score node for task delegation.
    /// score(n) = capability_match × (1 - load) × (1 / latency_placeholder)
    pub fn score_node(&self, node: &NodeDescriptor, required_caps: &[String]) -> f64 {
        let cap_match = if required_caps.is_empty() {
            1.0
        } else {
            let matched = required_caps
                .iter()
                .filter(|cap| node.capabilities.contains(cap))
                .count();
            matched as f64 / required_caps.len() as f64
        };
        let load_factor = 1.0 - node.load.min(1.0);
        cap_match * load_factor
    }
}

/// The mesh daemon — manages node lifecycle, discovery, and delegation.
pub struct MeshDaemon {
    pub local_node: NodeDescriptor,
    pub registry: Arc<RwLock<NodeRegistry>>,
    pub crdt_store: Arc<RwLock<super::CrdtStore>>,
}

impl MeshDaemon {
    pub fn new(local_node: NodeDescriptor) -> Self {
        let mut registry = NodeRegistry::new();
        registry.upsert(local_node.clone());
        Self {
            local_node,
            registry: Arc::new(RwLock::new(registry)),
            crdt_store: Arc::new(RwLock::new(super::CrdtStore::new())),
        }
    }

    /// Start the mesh daemon (gossip listener + mDNS advertiser).
    pub async fn start(&self, bind_addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
        tracing::info!(addr = %bind_addr, node = %self.local_node.id, "Mesh daemon starting");

        let registry = self.registry.clone();
        let local = self.local_node.clone();

        // Spawn TCP listener for gossip messages
        let listener = tokio::net::TcpListener::bind(bind_addr).await?;
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer)) => {
                        let registry = registry.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_peer(stream, peer, registry).await {
                                tracing::debug!(peer = %peer, error = %e, "Peer handler error");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Accept error");
                    }
                }
            }
        });

        // Spawn periodic gossip sender
        let registry = self.registry.clone();
        let local_desc = self.local_node.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                let reg = registry.read().await;
                let targets: Vec<SocketAddr> = reg
                    .alive_nodes()
                    .iter()
                    .filter(|n| n.id != local_desc.id)
                    .map(|n| n.addr)
                    .collect();
                drop(reg);

                for target in targets.iter().take(3) {
                    // SWIM: ping random subset
                    let msg = super::swim::SwimMessage::Ping(local_desc.clone());
                    let _ = super::swim::send_message(*target, &msg).await;
                }
            }
        });

        Ok(())
    }

    /// Join a mesh via a seed node address.
    pub async fn join(&self, seed: SocketAddr) -> Result<(), String> {
        let msg = super::swim::SwimMessage::Join(self.local_node.clone());
        super::swim::send_message(seed, &msg).await?;
        tracing::info!(seed = %seed, "Sent join request to mesh");
        Ok(())
    }
}

async fn handle_peer(
    mut stream: tokio::net::TcpStream,
    peer: SocketAddr,
    registry: Arc<RwLock<NodeRegistry>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; 65536];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }

    let msg: super::swim::SwimMessage = serde_json::from_slice(&buf[..n])?;
    match msg {
        super::swim::SwimMessage::Ping(desc) | super::swim::SwimMessage::Join(desc) => {
            let mut reg = registry.write().await;
            reg.upsert(desc);
        }
        super::swim::SwimMessage::PingReq { target, .. } => {
            tracing::debug!(target = %target, "Received ping-req, forwarding");
        }
        super::swim::SwimMessage::Suspect(id) => {
            let mut reg = registry.write().await;
            reg.mark_suspect(&id);
        }
        super::swim::SwimMessage::Dead(id) => {
            let mut reg = registry.write().await;
            reg.mark_dead(&id);
        }
        super::swim::SwimMessage::Sync(nodes) => {
            let mut reg = registry.write().await;
            for desc in nodes {
                reg.upsert(desc);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn make_node(id: &str, caps: &[&str], load: f64) -> NodeDescriptor {
        NodeDescriptor {
            id: id.to_string(),
            name: id.to_string(),
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9000),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            model: Some("test-model".to_string()),
            load,
            gpu: None,
            project_roots: Vec::new(),
            joined_at: chrono::Utc::now(),
            last_heartbeat: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_registry_find_by_capability() {
        let mut reg = NodeRegistry::new();
        reg.upsert(make_node("gpu1", &["ml", "cuda"], 0.1));
        reg.upsert(make_node("ci1", &["docker", "test"], 0.5));
        reg.upsert(make_node("dev1", &["rust", "python"], 0.3));

        let ml_nodes = reg.find_by_capability(&["ml".to_string()]);
        assert_eq!(ml_nodes.len(), 1);
        assert_eq!(ml_nodes[0].id, "gpu1");
    }

    #[test]
    fn test_registry_scoring() {
        let reg = NodeRegistry::new();
        let gpu = make_node("gpu1", &["ml", "cuda"], 0.1);
        let busy = make_node("busy", &["ml", "cuda"], 0.9);

        let score_gpu = reg.score_node(&gpu, &["ml".to_string()]);
        let score_busy = reg.score_node(&busy, &["ml".to_string()]);
        assert!(score_gpu > score_busy, "Low-load node should score higher");
    }
}
