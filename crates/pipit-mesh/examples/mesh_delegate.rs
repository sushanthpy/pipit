//! Mesh task delegator — sends a coding task to a remote mesh node.
//!
//! Usage:
//!   mesh_delegate <target_ip:port> "task prompt" [project_root]
//!
//! Example:
//!   mesh_delegate 100.127.255.44:4190 "Write tests for calculator.py" /tmp/mesh-project

use pipit_mesh::{MeshMessage, MeshTask, MeshTaskResult};
use pipit_mesh::swim::{write_mesh_message, read_mesh_message};
use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    if std::env::var("RUST_LOG").is_err() {
        unsafe { std::env::set_var("RUST_LOG", "info") };
    }
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: mesh_delegate <target_ip:port> \"task prompt\" [project_root]");
        std::process::exit(1);
    }

    let target: SocketAddr = args[1].parse().expect("Invalid target address");
    let prompt = &args[2];
    let project_root = args.get(3).cloned().unwrap_or_else(|| "/tmp".to_string());

    let task = MeshTask {
        id: uuid::Uuid::new_v4().to_string(),
        prompt: prompt.to_string(),
        required_capabilities: vec!["agent".to_string()],
        project_root: Some(project_root.clone()),
        timeout_secs: 300,
    };

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Mesh Task Delegation");
    println!("  Target:  {}", target);
    println!("  Root:    {}", project_root);
    println!("  Prompt:  {}", &prompt[..prompt.len().min(100)]);
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("\nSending task...");

    let mut stream = tokio::net::TcpStream::connect(target)
        .await
        .expect("Failed to connect to target");
    
    let msg = MeshMessage::TaskRequest(task);
    write_mesh_message(&mut stream, &msg)
        .await
        .expect("Failed to send task");
    println!("✓ Task sent, waiting for result...\n");

    match tokio::time::timeout(
        std::time::Duration::from_secs(300),
        read_mesh_message(&mut stream),
    )
    .await
    {
        Ok(Ok(MeshMessage::TaskResult(result))) => {
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            println!("  Result");
            println!("  Node:    {}", if result.node_id.is_empty() { "remote" } else { &result.node_id });
            println!("  Status:  {}", if result.success { "✓ Success" } else { "✗ Failed" });
            println!("  Time:    {:.1}s", result.elapsed_secs);
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            println!("\n{}", result.output);
        }
        Ok(Ok(other)) => {
            eprintln!("Unexpected response: {:?}", other);
        }
        Ok(Err(e)) => {
            eprintln!("Failed to read response: {}", e);
        }
        Err(_) => {
            eprintln!("Task timed out (5 min)");
        }
    }
}
