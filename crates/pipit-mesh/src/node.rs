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

    /// Find a node by ID prefix or name prefix (case-insensitive).
    pub fn find_by_prefix(&self, prefix: &str) -> Option<&NodeDescriptor> {
        let lower = prefix.to_lowercase();
        self.nodes.values()
            .find(|(d, s)| *s == NodeStatus::Alive && (d.id.to_lowercase().starts_with(&lower) || d.name.to_lowercase().starts_with(&lower)))
            .map(|(d, _)| d)
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

use std::future::Future;
use std::pin::Pin;

/// Task handler: receives a MeshTask, returns a MeshTaskResult.
pub type TaskHandler = Arc<
    dyn Fn(super::delegation::MeshTask) -> Pin<Box<dyn Future<Output = super::delegation::MeshTaskResult> + Send>>
        + Send
        + Sync,
>;

/// The mesh daemon — manages node lifecycle, discovery, and delegation.
pub struct MeshDaemon {
    pub local_node: NodeDescriptor,
    pub registry: Arc<RwLock<NodeRegistry>>,
    pub crdt_store: Arc<RwLock<super::CrdtStore>>,
    pub task_handler: Option<TaskHandler>,
}

impl MeshDaemon {
    pub fn new(local_node: NodeDescriptor) -> Self {
        let mut registry = NodeRegistry::new();
        registry.upsert(local_node.clone());
        Self {
            local_node,
            registry: Arc::new(RwLock::new(registry)),
            crdt_store: Arc::new(RwLock::new(super::CrdtStore::new())),
            task_handler: None,
        }
    }

    /// Set the task handler for processing delegated tasks.
    pub fn set_task_handler(&mut self, handler: TaskHandler) {
        self.task_handler = Some(handler);
    }

    /// Start the mesh daemon (gossip listener + task handler).
    pub async fn start(&self, bind_addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
        tracing::info!(addr = %bind_addr, node = %self.local_node.id, "Mesh daemon starting");

        let registry = self.registry.clone();
        let task_handler = self.task_handler.clone();

        // Spawn TCP listener for gossip + task messages
        let listener = tokio::net::TcpListener::bind(bind_addr).await?;
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer)) => {
                        let registry = registry.clone();
                        let handler = task_handler.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_peer(stream, peer, registry, handler).await {
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

        // Spawn periodic gossip sender with failure detection
        let registry = self.registry.clone();
        let local_desc = self.local_node.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            // Track consecutive ping failures per node for suspect/dead detection
            let mut fail_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
            loop {
                interval.tick().await;
                let reg = registry.read().await;
                let targets: Vec<(String, SocketAddr)> = reg
                    .all_nodes()
                    .into_iter()
                    .filter(|(n, s)| n.id != local_desc.id && **s != super::node::NodeStatus::Dead)
                    .map(|(n, _)| (n.id.clone(), n.addr))
                    .collect();
                // Build sync payload (all known nodes)
                let all_descs: Vec<super::node::NodeDescriptor> = reg
                    .all_nodes()
                    .into_iter()
                    .map(|(d, _)| d.clone())
                    .collect();
                drop(reg);

                for (id, target) in targets.iter().take(3) {
                    // SWIM: ping random subset
                    let ping = super::swim::SwimMessage::Ping(local_desc.clone());
                    match super::swim::send_message(*target, &ping).await {
                        Ok(_) => {
                            // Ping succeeded — reset fail counter
                            fail_counts.remove(id);
                            // Send Sync with full node list for transitive discovery
                            let sync = super::swim::SwimMessage::Sync(all_descs.clone());
                            let _ = super::swim::send_message(*target, &sync).await;
                        }
                        Err(_) => {
                            let count = fail_counts.entry(id.clone()).or_insert(0);
                            *count += 1;
                            if *count >= 3 {
                                // 3+ consecutive failures → mark Dead
                                tracing::info!(node = %&id[..8], addr = %target, "Node marked Dead (3 failed pings)");
                                let mut reg = registry.write().await;
                                reg.mark_dead(id);
                                fail_counts.remove(id);
                            } else if *count >= 1 {
                                // 1-2 failures → mark Suspect
                                tracing::debug!(node = %&id[..8], addr = %target, fails = *count, "Node suspected");
                                let mut reg = registry.write().await;
                                reg.mark_suspect(id);
                            }
                        }
                    }
                }

                // Evict dead nodes
                let mut reg = registry.write().await;
                reg.evict_dead();
            }
        });

        Ok(())
    }

    /// Join a mesh via a seed node address.
    pub async fn join(&self, seed: SocketAddr) -> Result<(), String> {
        use super::swim::{MeshMessage, write_mesh_message, read_mesh_message};
        let msg = MeshMessage::Swim(super::swim::SwimMessage::Join(self.local_node.clone()));

        let mut stream = tokio::net::TcpStream::connect(seed)
            .await
            .map_err(|e| format!("Connect to {}: {}", seed, e))?;
        write_mesh_message(&mut stream, &msg).await?;

        // Read Sync response with existing mesh nodes
        match tokio::time::timeout(std::time::Duration::from_secs(5), read_mesh_message(&mut stream)).await
        {
            Ok(Ok(MeshMessage::Swim(super::swim::SwimMessage::Sync(nodes)))) => {
                let mut reg = self.registry.write().await;
                for desc in nodes {
                    reg.upsert(desc);
                }
                tracing::info!(
                    seed = %seed,
                    nodes_received = reg.all_nodes().len(),
                    "Joined mesh and received node list"
                );
            }
            _ => {
                tracing::info!(seed = %seed, "Joined mesh (no sync response)");
            }
        }
        Ok(())
    }

    /// Delegate a task to the best available node in the mesh.
    /// Returns the task result from the remote node.
    pub async fn delegate_task(
        &self,
        task: super::delegation::MeshTask,
    ) -> Result<super::delegation::MeshTaskResult, String> {
        use super::swim::{MeshMessage, write_mesh_message, read_mesh_message};

        // Find best node (not self)
        let reg = self.registry.read().await;
        let candidates: Vec<&NodeDescriptor> = reg
            .alive_nodes()
            .into_iter()
            .filter(|n| n.id != self.local_node.id)
            .collect();

        if candidates.is_empty() {
            return Err("No remote nodes available in mesh".to_string());
        }

        // Score nodes: prefer nodes with matching capabilities, lower load
        let delegation = super::delegation::MeshDelegation::new();
        let target = delegation
            .select_node(&task, &reg)
            .filter(|n| n.id != self.local_node.id)
            .or_else(|| candidates.first().copied())
            .ok_or("No suitable node found")?
            .clone();
        drop(reg);

        tracing::info!(
            target_node = %&target.id[..8],
            target_name = %target.name,
            target_addr = %target.addr,
            task_id = %task.id,
            "Delegating task to mesh node"
        );

        // Send task
        let msg = MeshMessage::TaskRequest(task);
        let mut stream = tokio::net::TcpStream::connect(target.addr)
            .await
            .map_err(|e| format!("Connect to {}: {}", target.addr, e))?;
        write_mesh_message(&mut stream, &msg).await?;

        // Wait for result (up to task timeout)
        let timeout = std::time::Duration::from_secs(300); // 5 min max
        match tokio::time::timeout(timeout, read_mesh_message(&mut stream)).await {
            Ok(Ok(MeshMessage::TaskResult(result))) => Ok(result),
            Ok(Ok(other)) => Err(format!("Unexpected response: {:?}", other)),
            Ok(Err(e)) => Err(format!("Read response: {}", e)),
            Err(_) => Err("Task delegation timed out (5 min)".to_string()),
        }
    }

    /// Delegate a task to a specific node (by address).
    pub async fn delegate_to_node(
        &self,
        task: super::delegation::MeshTask,
        target_addr: SocketAddr,
    ) -> Result<super::delegation::MeshTaskResult, String> {
        use super::swim::{MeshMessage, write_mesh_message, read_mesh_message};

        let msg = MeshMessage::TaskRequest(task);
        let mut stream = tokio::net::TcpStream::connect(target_addr)
            .await
            .map_err(|e| format!("Connect to {}: {}", target_addr, e))?;
        write_mesh_message(&mut stream, &msg).await?;

        let timeout = std::time::Duration::from_secs(300);
        match tokio::time::timeout(timeout, read_mesh_message(&mut stream)).await {
            Ok(Ok(MeshMessage::TaskResult(result))) => Ok(result),
            Ok(Ok(other)) => Err(format!("Unexpected response: {:?}", other)),
            Ok(Err(e)) => Err(format!("Read response: {}", e)),
            Err(_) => Err("Task delegation timed out (5 min)".to_string()),
        }
    }

    /// Broadcast a task to all alive remote nodes in parallel.
    pub async fn broadcast_task(
        &self,
        task: super::delegation::MeshTask,
    ) -> Vec<Result<super::delegation::MeshTaskResult, String>> {
        use super::swim::{MeshMessage, write_mesh_message, read_mesh_message};

        let reg = self.registry.read().await;
        let remotes: Vec<SocketAddr> = reg
            .alive_nodes()
            .into_iter()
            .filter(|n| n.id != self.local_node.id)
            .map(|n| n.addr)
            .collect();
        drop(reg);

        let mut handles = Vec::new();
        for addr in remotes {
            let t = task.clone();
            handles.push(tokio::spawn(async move {
                let msg = MeshMessage::TaskRequest(t);
                let mut stream = tokio::net::TcpStream::connect(addr)
                    .await
                    .map_err(|e| format!("Connect to {}: {}", addr, e))?;
                write_mesh_message(&mut stream, &msg).await?;

                let timeout = std::time::Duration::from_secs(300);
                match tokio::time::timeout(timeout, read_mesh_message(&mut stream)).await {
                    Ok(Ok(MeshMessage::TaskResult(result))) => Ok(result),
                    Ok(Ok(other)) => Err(format!("Unexpected response: {:?}", other)),
                    Ok(Err(e)) => Err(format!("Read response: {}", e)),
                    Err(_) => Err("Task delegation timed out (5 min)".to_string()),
                }
            }));
        }

        let mut results = Vec::new();
        for h in handles {
            match h.await {
                Ok(r) => results.push(r),
                Err(e) => results.push(Err(format!("Join error: {}", e))),
            }
        }
        results
    }
}

async fn handle_peer(
    mut stream: tokio::net::TcpStream,
    peer: SocketAddr,
    registry: Arc<RwLock<NodeRegistry>>,
    task_handler: Option<TaskHandler>,
) -> Result<(), Box<dyn std::error::Error>> {
    use super::swim::{MeshMessage, write_mesh_message, read_mesh_message};

    let msg = read_mesh_message(&mut stream).await
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    match msg {
        MeshMessage::Swim(swim_msg) => {
            match swim_msg {
                super::swim::SwimMessage::Ping(desc) => {
                    let mut reg = registry.write().await;
                    reg.upsert(desc);
                }
                super::swim::SwimMessage::Join(desc) => {
                    let mut reg = registry.write().await;
                    reg.upsert(desc);
                    let all_descs: Vec<NodeDescriptor> = reg
                        .all_nodes()
                        .into_iter()
                        .map(|(d, _)| d.clone())
                        .collect();
                    drop(reg);
                    let sync = MeshMessage::Swim(super::swim::SwimMessage::Sync(all_descs));
                    let _ = write_mesh_message(&mut stream, &sync).await;
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
        }
        MeshMessage::TaskRequest(task) => {
            tracing::info!(task_id = %task.id, prompt_len = task.prompt.len(), "Received delegated task");
            if let Some(ref handler) = task_handler {
                let result = handler(task).await;
                let _ = write_mesh_message(&mut stream, &MeshMessage::TaskResult(result)).await;
            } else {
                // No handler — run pipit as subprocess (default)
                let result = execute_task_subprocess(&task).await;
                let _ = write_mesh_message(&mut stream, &MeshMessage::TaskResult(result)).await;
            }
        }
        MeshMessage::TaskResult(_) => {
            tracing::warn!(peer = %peer, "Unexpected TaskResult from peer");
        }
    }
    Ok(())
}

/// Default task executor: run pipit as a subprocess.
async fn execute_task_subprocess(
    task: &super::delegation::MeshTask,
) -> super::delegation::MeshTaskResult {
    let start = std::time::Instant::now();
    let project_root = task.project_root.clone().unwrap_or_else(|| "/tmp".to_string());

    // Find pipit binary
    let pipit = which_pipit();

    tracing::info!(
        task_id = %task.id,
        project_root = %project_root,
        pipit = %pipit,
        "Executing delegated task via subprocess"
    );

    let result = tokio::process::Command::new(&pipit)
        .args([
            "-a", "full_auto",
            "--max-turns", "30",
            &task.prompt,
        ])
        .current_dir(&project_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;

    let elapsed = start.elapsed().as_secs_f64();

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let success = output.status.success();
            let combined = if stdout.is_empty() {
                stderr.to_string()
            } else {
                format!("{}\n{}", stdout, stderr)
            };
            // Truncate to reasonable size
            let output_str = if combined.len() > 50_000 {
                format!("{}...(truncated)", &combined[..50_000])
            } else {
                combined
            };
            super::delegation::MeshTaskResult {
                task_id: task.id.clone(),
                node_id: String::new(), // filled by caller if needed
                success,
                output: output_str,
                elapsed_secs: elapsed,
                cost_usd: 0.0,
            }
        }
        Err(e) => super::delegation::MeshTaskResult {
            task_id: task.id.clone(),
            node_id: String::new(),
            success: false,
            output: format!("Failed to execute pipit: {}", e),
            elapsed_secs: elapsed,
            cost_usd: 0.0,
        },
    }
}

/// Find the pipit binary.
fn which_pipit() -> String {
    for path in &["/usr/local/bin/pipit", "/usr/bin/pipit"] {
        if std::path::Path::new(path).exists() {
            return path.to_string();
        }
    }
    // Check ~/.local/bin
    if let Ok(home) = std::env::var("HOME") {
        let local = format!("{}/.local/bin/pipit", home);
        if std::path::Path::new(&local).exists() {
            return local;
        }
    }
    "pipit".to_string() // fallback to PATH
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
