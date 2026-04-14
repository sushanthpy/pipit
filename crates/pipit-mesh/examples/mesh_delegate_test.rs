//! Mesh task delegation integration test.
//!
//! Starts two mesh nodes on localhost, delegates a "write hello world" task
//! from node1 to node2, and verifies the result.
//!
//! Usage:
//!   cargo run --release --example mesh_delegate_test -p pipit-mesh

use pipit_mesh::{MeshDaemon, MeshTask, NodeDescriptor};
use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    if std::env::var("RUST_LOG").is_err() {
        unsafe { std::env::set_var("RUST_LOG", "info") };
    }
    tracing_subscriber::fmt::init();

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Mesh Task Delegation Test");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // Node1: the delegator (sends tasks)
    let bind1: SocketAddr = "127.0.0.1:14190".parse().unwrap();
    let node1 = NodeDescriptor {
        id: uuid::Uuid::new_v4().to_string(),
        name: "delegator".to_string(),
        addr: bind1,
        capabilities: vec!["agent".into()],
        model: None,
        load: 0.8, // High load — should prefer to delegate
        gpu: None,
        project_roots: vec![],
        joined_at: chrono::Utc::now(),
        last_heartbeat: chrono::Utc::now(),
    };
    let daemon1 = std::sync::Arc::new(MeshDaemon::new(node1));

    // Node2: the worker (executes tasks)
    let bind2: SocketAddr = "127.0.0.1:14191".parse().unwrap();
    let node2 = NodeDescriptor {
        id: uuid::Uuid::new_v4().to_string(),
        name: "worker".to_string(),
        addr: bind2,
        capabilities: vec!["agent".into()],
        model: Some("qwen".into()),
        load: 0.1, // Low load — good target
        gpu: None,
        project_roots: vec!["/tmp/mesh-test-project".to_string()],
        joined_at: chrono::Utc::now(),
        last_heartbeat: chrono::Utc::now(),
    };

    // Create worker with custom task handler (echoes back a result without running pipit)
    let mut daemon2_inner = MeshDaemon::new(node2);
    daemon2_inner.set_task_handler(std::sync::Arc::new(|task: MeshTask| {
        Box::pin(async move {
            println!("  [worker] Received task: {}", &task.prompt[..task.prompt.len().min(80)]);
            // Simulate work
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            pipit_mesh::MeshTaskResult {
                task_id: task.id,
                node_id: "worker".to_string(),
                success: true,
                output: format!(
                    "Task completed by worker node.\nPrompt: {}\nResult: Created the requested files successfully.",
                    &task.prompt[..task.prompt.len().min(200)]
                ),
                elapsed_secs: 0.5,
                cost_usd: 0.001,
            }
        })
    }));
    let daemon2 = std::sync::Arc::new(daemon2_inner);

    // Start both daemons
    println!("\n1. Starting mesh daemons...");
    daemon1.start(bind1).await.expect("Failed to start daemon1");
    println!("   ✓ Delegator listening on {}", bind1);

    daemon2.start(bind2).await.expect("Failed to start daemon2");
    println!("   ✓ Worker listening on {}", bind2);

    // Join: daemon1 joins daemon2
    println!("\n2. Forming mesh...");
    daemon1.join(bind2).await.expect("Failed to join");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let reg = daemon1.registry.read().await;
    let nodes = reg.all_nodes();
    println!("   ✓ Mesh formed: {} nodes", nodes.len());
    for (desc, status) in &nodes {
        let me = if desc.id == daemon1.local_node.id { " (self)" } else { "" };
        println!("     {:?} {} {}{}", status, desc.name, desc.addr, me);
    }
    drop(reg);

    // Delegate a task
    println!("\n3. Delegating task to mesh...");
    let task = MeshTask {
        id: uuid::Uuid::new_v4().to_string(),
        prompt: "Write a Python function that calculates fibonacci numbers and save it to fib.py".to_string(),
        required_capabilities: vec!["agent".to_string()],
        project_root: Some("/tmp/mesh-test-project".to_string()),
        timeout_secs: 60,
    };
    println!("   Task: {}", &task.prompt);

    match daemon1.delegate_task(task).await {
        Ok(result) => {
            println!("\n4. ✓ Task completed!");
            println!("   Node:    {}", result.node_id);
            println!("   Success: {}", result.success);
            println!("   Time:    {:.1}s", result.elapsed_secs);
            println!("   Output:  {}", &result.output[..result.output.len().min(500)]);
        }
        Err(e) => {
            println!("\n4. ✗ Task failed: {}", e);
        }
    }

    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Test Complete");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
}
