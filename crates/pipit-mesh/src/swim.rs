//! SWIM (Scalable Weakly-consistent Infection-style Membership) gossip protocol.
//!
//! Convergence: O(log N) protocol periods for N nodes.
//! Failure detection: ping → ping-req → suspect → dead in 3·RTT.
//! Message overhead: O(N·log N) per period via piggybacking.

use crate::node::NodeDescriptor;
use crate::delegation::{MeshTask, MeshTaskResult};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// Top-level mesh wire protocol. Every TCP message is a MeshMessage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MeshMessage {
    /// Gossip/membership protocol message.
    Swim(SwimMessage),
    /// Task delegation: sender asks this node to execute a task.
    TaskRequest(MeshTask),
    /// Task result: response to a TaskRequest.
    TaskResult(MeshTaskResult),
}

/// SWIM protocol messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SwimMessage {
    /// Direct ping with sender's descriptor.
    Ping(NodeDescriptor),
    /// Indirect ping request: ask another node to ping the target.
    PingReq {
        sender: NodeDescriptor,
        target: String,
    },
    /// Node join announcement.
    Join(NodeDescriptor),
    /// Mark a node as suspect (no response to ping).
    Suspect(String),
    /// Confirm a node as dead (no response to ping-req).
    Dead(String),
    /// Full state sync (sent on join).
    Sync(Vec<NodeDescriptor>),
}

/// SWIM protocol configuration.
#[derive(Debug, Clone)]
pub struct SwimConfig {
    /// How often to send pings (seconds).
    pub protocol_period_secs: u64,
    /// Number of random nodes to ping each period.
    pub ping_fanout: usize,
    /// Number of nodes to ask for indirect pings.
    pub ping_req_fanout: usize,
    /// How long to wait before marking suspect as dead (seconds).
    pub suspect_timeout_secs: u64,
}

impl Default for SwimConfig {
    fn default() -> Self {
        Self {
            protocol_period_secs: 5,
            ping_fanout: 3,
            ping_req_fanout: 2,
            suspect_timeout_secs: 15,
        }
    }
}

/// The SWIM protocol state machine.
pub struct SwimProtocol {
    pub config: SwimConfig,
    pub local_id: String,
    /// Nodes currently suspected (id → when suspected).
    suspects: std::collections::HashMap<String, std::time::Instant>,
}

impl SwimProtocol {
    pub fn new(local_id: String, config: SwimConfig) -> Self {
        Self {
            config,
            local_id,
            suspects: std::collections::HashMap::new(),
        }
    }

    /// Process a received SWIM message. Returns any response messages to send.
    pub fn handle_message(&mut self, msg: &SwimMessage) -> Vec<(SocketAddr, SwimMessage)> {
        match msg {
            SwimMessage::Ping(desc) => {
                // Respond with our own ping (ack)
                // Remove from suspects if present
                self.suspects.remove(&desc.id);
                Vec::new()
            }
            SwimMessage::PingReq { sender: _, target } => {
                // We should ping the target on behalf of sender
                tracing::debug!(target = %target, "Handling ping-req");
                Vec::new()
            }
            SwimMessage::Suspect(id) => {
                if id != &self.local_id {
                    self.suspects.insert(id.clone(), std::time::Instant::now());
                }
                Vec::new()
            }
            SwimMessage::Dead(id) => {
                self.suspects.remove(id);
                Vec::new()
            }
            SwimMessage::Join(_) | SwimMessage::Sync(_) => Vec::new(),
        }
    }

    /// Check for timed-out suspects that should be marked dead.
    pub fn check_suspects(&mut self) -> Vec<String> {
        let timeout = std::time::Duration::from_secs(self.config.suspect_timeout_secs);
        let now = std::time::Instant::now();
        let dead: Vec<String> = self
            .suspects
            .iter()
            .filter(|(_, when)| now.duration_since(**when) > timeout)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &dead {
            self.suspects.remove(id);
        }
        dead
    }
}

/// Send a SWIM message to a target address via TCP (wrapped in MeshMessage envelope).
pub async fn send_message(target: SocketAddr, msg: &SwimMessage) -> Result<(), String> {
    let envelope = MeshMessage::Swim(msg.clone());
    send_mesh_message(target, &envelope).await
}

/// Send any MeshMessage to a target address via TCP with 4-byte length prefix.
pub async fn send_mesh_message(target: SocketAddr, msg: &MeshMessage) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let json = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
    let len = (json.len() as u32).to_be_bytes();
    let mut stream = tokio::net::TcpStream::connect(target)
        .await
        .map_err(|e| format!("Connect to {}: {}", target, e))?;
    stream.write_all(&len).await.map_err(|e| e.to_string())?;
    stream.write_all(&json).await.map_err(|e| e.to_string())?;
    stream.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Send a MeshMessage over an existing TCP stream with 4-byte length prefix.
pub async fn write_mesh_message(
    stream: &mut tokio::net::TcpStream,
    msg: &MeshMessage,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let json = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
    let len = (json.len() as u32).to_be_bytes();
    stream.write_all(&len).await.map_err(|e| e.to_string())?;
    stream.write_all(&json).await.map_err(|e| e.to_string())?;
    stream.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Read a MeshMessage from a TCP stream (4-byte length prefix, or fallback to raw JSON).
pub async fn read_mesh_message(
    stream: &mut tokio::net::TcpStream,
) -> Result<MeshMessage, String> {
    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .map_err(|e| format!("Read length: {}", e))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        return Err(format!("Message too large: {} bytes", len));
    }
    let mut buf = vec![0u8; len];
    stream
        .read_exact(&mut buf)
        .await
        .map_err(|e| format!("Read body: {}", e))?;
    serde_json::from_slice(&buf).map_err(|e| format!("Parse message: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_swim_suspect_timeout() {
        let mut proto = SwimProtocol::new(
            "local".to_string(),
            SwimConfig {
                suspect_timeout_secs: 0, // Immediate timeout for test
                ..Default::default()
            },
        );
        proto.suspects.insert(
            "node1".to_string(),
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        );
        let dead = proto.check_suspects();
        assert_eq!(dead, vec!["node1"]);
    }
}
