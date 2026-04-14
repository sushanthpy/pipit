//! Standalone mesh connectivity tester.
//!
//! Usage:
//!   mesh_test <bind_addr> [seed_addr]
//!
//! Examples:
//!   # Start seed node:
//!   mesh_test 192.168.1.191:4190
//!
//!   # Join existing mesh:
//!   mesh_test 192.168.1.198:4190 192.168.1.191:4190

use pipit_mesh::{MeshDaemon, NodeDescriptor};
use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    // Simple env-based logging
    if std::env::var("RUST_LOG").is_err() {
        unsafe { std::env::set_var("RUST_LOG", "info") };
    }
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: mesh_test <bind_ip:port> [seed_ip:port]");
        eprintln!("  e.g. mesh_test 192.168.1.191:4190");
        eprintln!("  e.g. mesh_test 0.0.0.0:4190 --advertise 100.127.255.44:4190");
        eprintln!("  e.g. mesh_test 0.0.0.0:4190 --advertise 100.109.115.80:4190 --seed 100.127.255.44:4190");
        std::process::exit(1);
    }

    // Parse args: mesh_test <bind> [--advertise <addr>] [--seed <addr>] [seed_positional]
    let bind_addr: SocketAddr = args[1].parse().expect("Invalid bind address");
    let mut advertise_addr = bind_addr;
    let mut seed: Option<SocketAddr> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--advertise" | "-a" => {
                i += 1;
                advertise_addr = args[i].parse().expect("Invalid advertise address");
            }
            "--seed" | "-s" => {
                i += 1;
                seed = Some(args[i].parse().expect("Invalid seed address"));
            }
            _ => {
                // Positional: treat as seed for backward compat
                seed = Some(args[i].parse().expect("Invalid seed address"));
            }
        }
        i += 1;
    }

    let hostname = std::process::Command::new("hostname")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let node = NodeDescriptor {
        id: uuid::Uuid::new_v4().to_string(),
        name: hostname.clone(),
        addr: advertise_addr,
        capabilities: vec!["agent".into(), "mesh-test".into()],
        model: None,
        load: 0.0,
        gpu: None,
        project_roots: vec![],
        joined_at: chrono::Utc::now(),
        last_heartbeat: chrono::Utc::now(),
    };

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("Mesh Test Node");
    println!("  Node ID:    {}", &node.id[..8]);
    println!("  Name:       {}", hostname);
    println!("  Bind:       {}", bind_addr);
    println!("  Advertise:  {}", advertise_addr);
    if let Some(s) = seed {
        println!("  Seed:       {}", s);
    }
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let daemon = MeshDaemon::new(node);

    // Start TCP listener + gossip
    if let Err(e) = daemon.start(bind_addr).await {
        eprintln!("FATAL: Failed to start mesh daemon: {}", e);
        std::process::exit(1);
    }
    println!("✓ Mesh daemon listening on {} (advertise: {})", bind_addr, advertise_addr);

    // Join seed if provided
    if let Some(seed_addr) = seed {
        println!("→ Joining seed {}...", seed_addr);
        match daemon.join(seed_addr).await {
            Ok(_) => println!("✓ Join message sent to {}", seed_addr),
            Err(e) => eprintln!("✗ Failed to join seed: {}", e),
        }
    }

    // Print registry every 5 seconds
    println!("\nMonitoring mesh (Ctrl+C to exit)...\n");
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
    loop {
        interval.tick().await;
        let reg = daemon.registry.read().await;
        let nodes = reg.all_nodes();
        let alive = nodes.iter().filter(|(_, s)| **s == pipit_mesh::NodeStatus::Alive).count();
        println!(
            "[{}] Nodes: {} total, {} alive",
            chrono::Utc::now().format("%H:%M:%S"),
            nodes.len(),
            alive
        );
        for (desc, status) in &nodes {
            let self_marker = if desc.id == daemon.local_node.id {
                " (self)"
            } else {
                ""
            };
            println!(
                "  {:?} {:8} {:22} {}{}",
                status,
                &desc.id[..8],
                desc.addr,
                desc.name,
                self_marker
            );
        }
    }
}
