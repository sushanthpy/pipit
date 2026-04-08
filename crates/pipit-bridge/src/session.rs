//! Bridge session management.
//!
//! Handles session creation, lifecycle, reconnection, and state tracking.

use crate::protocol::{BridgeCommand, BridgeEvent, BridgeMessage, BridgePayload, MessageId};
use crate::transport::{LamportClock, Transport, TransportError};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast, mpsc};

/// Session configuration.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Unique session identifier.
    pub session_id: String,
    /// Maximum events to buffer for slow consumers.
    pub event_buffer_size: usize,
    /// Timeout for idle sessions (seconds). 0 = no timeout.
    pub idle_timeout_secs: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            session_id: uuid::Uuid::new_v4().to_string(),
            event_buffer_size: 1024,
            idle_timeout_secs: 3600,
        }
    }
}

/// A bridge session managing communication between agent and IDE.
pub struct BridgeSession {
    config: SessionConfig,
    transport: Arc<dyn Transport>,
    clock: Mutex<LamportClock>,
    /// Channel for receiving commands from transport.   
    command_rx: Mutex<mpsc::Receiver<BridgeCommand>>,
    /// Channel for sending commands into the session.
    command_tx: mpsc::Sender<BridgeCommand>,
    /// Broadcast channel for events to all subscribers.
    event_tx: broadcast::Sender<BridgeEvent>,
    active: std::sync::atomic::AtomicBool,
}

impl BridgeSession {
    pub fn new(config: SessionConfig, transport: Arc<dyn Transport>) -> Self {
        let (command_tx, command_rx) = mpsc::channel(256);
        let (event_tx, _) = broadcast::channel(config.event_buffer_size);

        Self {
            config,
            transport,
            clock: Mutex::new(LamportClock::new()),
            command_rx: Mutex::new(command_rx),
            command_tx,
            event_tx,
            active: std::sync::atomic::AtomicBool::new(true),
        }
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &str {
        &self.config.session_id
    }

    /// Check if the session is active.
    pub fn is_active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Emit an event to the IDE.
    pub async fn emit_event(&self, event: BridgeEvent) -> Result<(), TransportError> {
        let mut clock = self.clock.lock().await;
        let id = clock.next();
        drop(clock);

        let message = BridgeMessage {
            id,
            payload: BridgePayload::Event(event.clone()),
        };

        self.transport.send(message).await?;
        let _ = self.event_tx.send(event);
        Ok(())
    }

    /// Subscribe to events (for internal consumers).
    pub fn subscribe_events(&self) -> broadcast::Receiver<BridgeEvent> {
        self.event_tx.subscribe()
    }

    /// Receive the next command from the IDE.
    pub async fn recv_command(&self) -> Option<BridgeCommand> {
        let mut rx = self.command_rx.lock().await;
        rx.recv().await
    }

    /// Start the message receive loop (spawns a background task).
    pub fn start_receive_loop(self: &Arc<Self>) {
        let session = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                if !session.is_active() {
                    break;
                }

                match session.transport.recv().await {
                    Ok(message) => {
                        // Update Lamport clock
                        let mut clock = session.clock.lock().await;
                        clock.receive(&message.id);
                        drop(clock);

                        match message.payload {
                            BridgePayload::Command(cmd) => {
                                let _ = session.command_tx.send(cmd).await;
                            }
                            BridgePayload::Heartbeat { .. } => {
                                // Respond with ack
                                let ack = BridgeMessage {
                                    id: {
                                        let mut c = session.clock.lock().await;
                                        c.next()
                                    },
                                    payload: BridgePayload::Ack { ack_id: message.id },
                                };
                                let _ = session.transport.send(ack).await;
                            }
                            BridgePayload::Ack { .. } => {
                                // Acknowledgement received — no action needed
                            }
                            BridgePayload::Event(_) => {
                                // Events from IDE are unexpected — ignore
                            }
                        }
                    }
                    Err(TransportError::ConnectionClosed) => {
                        session
                            .active
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                        break;
                    }
                    Err(_) => {
                        // Transient error — continue
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        });
    }

    /// Close the session.
    pub async fn close(&self) -> Result<(), TransportError> {
        self.active
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.transport.close().await
    }
}
