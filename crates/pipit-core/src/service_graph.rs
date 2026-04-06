//! Runtime Service Graph
//!
//! Replaces ad-hoc feature flag gates with a typed dependency DAG.
//! Services declare dependencies, permission requirements, and lifecycle
//! hooks. The graph is topologically sorted at startup in O(V+E).
//! Service discovery is O(1) by registry lookup.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

// ─── Service Descriptor ─────────────────────────────────────────────────

/// A service in the runtime graph — replaces scattered feature gates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDescriptor {
    /// Unique service identifier (e.g., "planning", "browser", "voice").
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Services this one depends on (must be started first).
    pub dependencies: Vec<String>,
    /// Capability set this service requires to operate.
    pub required_capabilities: u32,
    /// Whether this service is enabled in the current configuration.
    pub enabled: bool,
    /// Lifecycle phase when this service should be initialized.
    pub phase: ServicePhase,
    /// Telemetry class for this service.
    pub telemetry_class: TelemetryClass,
}

/// When in the startup sequence this service initializes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ServicePhase {
    /// Core services (context, tools, permissions).
    Core,
    /// Standard services (planning, verification).
    Standard,
    /// Extended services (browser, voice, mesh).
    Extended,
    /// Optional services (telemetry, skills).
    Optional,
}

/// Telemetry classification for service events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TelemetryClass {
    /// Core runtime events — always collected.
    Core,
    /// Performance metrics — collected in debug/profile modes.
    Performance,
    /// Feature usage stats — collected for product analytics.
    Usage,
    /// Debug-level events — only in development.
    Debug,
}

// ─── Service Graph ──────────────────────────────────────────────────────

/// The runtime service graph. Resolves dependencies and provides
/// O(1) service discovery.
pub struct ServiceGraph {
    /// All registered services by ID.
    services: HashMap<String, ServiceDescriptor>,
    /// Topological order computed at startup. O(V+E) once.
    startup_order: Vec<String>,
    /// Shutdown order (reverse of startup).
    shutdown_order: Vec<String>,
    /// Services that failed to initialize.
    failed: HashSet<String>,
    /// Whether the graph has been resolved.
    resolved: bool,
}

/// Errors during graph resolution.
#[derive(Debug, Clone)]
pub enum ServiceGraphError {
    /// A dependency cycle was detected.
    CyclicDependency { cycle: Vec<String> },
    /// A required dependency is missing.
    MissingDependency { service: String, dependency: String },
    /// A dependency is disabled but required.
    DisabledDependency { service: String, dependency: String },
}

impl std::fmt::Display for ServiceGraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CyclicDependency { cycle } => {
                write!(f, "cyclic dependency: {}", cycle.join(" → "))
            }
            Self::MissingDependency {
                service,
                dependency,
            } => write!(f, "'{}' requires missing service '{}'", service, dependency),
            Self::DisabledDependency {
                service,
                dependency,
            } => write!(
                f,
                "'{}' requires disabled service '{}'",
                service, dependency
            ),
        }
    }
}

impl ServiceGraph {
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
            startup_order: Vec::new(),
            shutdown_order: Vec::new(),
            failed: HashSet::new(),
            resolved: false,
        }
    }

    /// Register a service descriptor.
    pub fn register(&mut self, desc: ServiceDescriptor) {
        self.services.insert(desc.id.clone(), desc);
        self.resolved = false;
    }

    /// Resolve the dependency graph: topological sort + validation.
    /// Cost: O(V + E) where V = services, E = dependency edges.
    pub fn resolve(&mut self) -> Result<(), Vec<ServiceGraphError>> {
        let mut errors = Vec::new();

        // Validate dependencies exist and are enabled
        for (id, desc) in &self.services {
            if !desc.enabled {
                continue;
            }
            for dep in &desc.dependencies {
                match self.services.get(dep) {
                    None => errors.push(ServiceGraphError::MissingDependency {
                        service: id.clone(),
                        dependency: dep.clone(),
                    }),
                    Some(dep_desc) if !dep_desc.enabled => {
                        errors.push(ServiceGraphError::DisabledDependency {
                            service: id.clone(),
                            dependency: dep.clone(),
                        })
                    }
                    _ => {}
                }
            }
        }

        if !errors.is_empty() {
            return Err(errors);
        }

        // Topological sort (Kahn's algorithm)
        let enabled: Vec<&str> = self
            .services
            .iter()
            .filter(|(_, d)| d.enabled)
            .map(|(id, _)| id.as_str())
            .collect();

        let mut in_degree: HashMap<&str, usize> = enabled.iter().map(|&id| (id, 0)).collect();
        let mut adj: HashMap<&str, Vec<&str>> = enabled.iter().map(|&id| (id, Vec::new())).collect();

        for &id in &enabled {
            let desc = &self.services[id];
            for dep in &desc.dependencies {
                if let Some(neighbors) = adj.get_mut(dep.as_str()) {
                    neighbors.push(id);
                }
                if let Some(degree) = in_degree.get_mut(id) {
                    *degree += 1;
                }
            }
        }

        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|(_, deg)| **deg == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut order = Vec::new();
        while let Some(node) = queue.pop_front() {
            order.push(node.to_string());
            if let Some(neighbors) = adj.get(node) {
                for &next in neighbors {
                    if let Some(degree) = in_degree.get_mut(next) {
                        *degree -= 1;
                        if *degree == 0 {
                            queue.push_back(next);
                        }
                    }
                }
            }
        }

        if order.len() != enabled.len() {
            // Cycle detected — find the cycle
            let remaining: Vec<String> = enabled
                .iter()
                .filter(|&&id| !order.contains(&id.to_string()))
                .map(|&id| id.to_string())
                .collect();
            errors.push(ServiceGraphError::CyclicDependency { cycle: remaining });
            return Err(errors);
        }

        // Sort by phase within topological order for deterministic startup
        order.sort_by(|a, b| {
            let pa = self.services[a].phase;
            let pb = self.services[b].phase;
            pa.cmp(&pb)
        });

        self.shutdown_order = order.iter().rev().cloned().collect();
        self.startup_order = order;
        self.resolved = true;
        Ok(())
    }

    /// Get the startup order. Panics if not resolved.
    pub fn startup_order(&self) -> &[String] {
        assert!(self.resolved, "service graph not resolved");
        &self.startup_order
    }

    /// Get the shutdown order. Panics if not resolved.
    pub fn shutdown_order(&self) -> &[String] {
        assert!(self.resolved, "service graph not resolved");
        &self.shutdown_order
    }

    /// Look up a service by ID. O(1) via HashMap.
    pub fn get(&self, id: &str) -> Option<&ServiceDescriptor> {
        self.services.get(id)
    }

    /// Check if a service is enabled and resolved. O(1).
    pub fn is_available(&self, id: &str) -> bool {
        self.services
            .get(id)
            .map(|d| d.enabled && !self.failed.contains(id))
            .unwrap_or(false)
    }

    /// Mark a service as failed.
    pub fn mark_failed(&mut self, id: &str) {
        self.failed.insert(id.to_string());
    }

    /// List all enabled services.
    pub fn enabled_services(&self) -> Vec<&ServiceDescriptor> {
        self.services.values().filter(|d| d.enabled).collect()
    }
}

// ─── Built-in Service Descriptors ───────────────────────────────────────

/// Register the standard set of pipit services.
pub fn register_builtin_services(graph: &mut ServiceGraph, feature_flags: &HashMap<String, bool>) {
    let flag = |name: &str| feature_flags.get(name).copied().unwrap_or(false);

    graph.register(ServiceDescriptor {
        id: "context".into(),
        name: "Context Manager".into(),
        dependencies: vec![],
        required_capabilities: 0,
        enabled: true,
        phase: ServicePhase::Core,
        telemetry_class: TelemetryClass::Core,
    });

    graph.register(ServiceDescriptor {
        id: "tools".into(),
        name: "Tool Registry".into(),
        dependencies: vec!["context".into()],
        required_capabilities: 0,
        enabled: true,
        phase: ServicePhase::Core,
        telemetry_class: TelemetryClass::Core,
    });

    graph.register(ServiceDescriptor {
        id: "permissions".into(),
        name: "Capability Kernel".into(),
        dependencies: vec![],
        required_capabilities: 0,
        enabled: true,
        phase: ServicePhase::Core,
        telemetry_class: TelemetryClass::Core,
    });

    graph.register(ServiceDescriptor {
        id: "planning".into(),
        name: "Plan IR + Deliberation".into(),
        dependencies: vec!["context".into(), "tools".into()],
        required_capabilities: 0,
        enabled: true,
        phase: ServicePhase::Standard,
        telemetry_class: TelemetryClass::Core,
    });

    graph.register(ServiceDescriptor {
        id: "verification".into(),
        name: "Verification Engine".into(),
        dependencies: vec!["tools".into()],
        required_capabilities: 0,
        enabled: true,
        phase: ServicePhase::Standard,
        telemetry_class: TelemetryClass::Core,
    });

    graph.register(ServiceDescriptor {
        id: "browser".into(),
        name: "Isolated Browser Twin".into(),
        dependencies: vec!["permissions".into()],
        required_capabilities: 0,
        enabled: flag("browser_tool"),
        phase: ServicePhase::Extended,
        telemetry_class: TelemetryClass::Usage,
    });

    graph.register(ServiceDescriptor {
        id: "voice".into(),
        name: "Duplex Speech Bus".into(),
        dependencies: vec!["context".into()],
        required_capabilities: 0,
        enabled: flag("voice_mode"),
        phase: ServicePhase::Extended,
        telemetry_class: TelemetryClass::Usage,
    });

    graph.register(ServiceDescriptor {
        id: "mesh".into(),
        name: "Agent Mesh".into(),
        dependencies: vec!["permissions".into(), "tools".into()],
        required_capabilities: 0,
        enabled: flag("agent_mesh"),
        phase: ServicePhase::Extended,
        telemetry_class: TelemetryClass::Usage,
    });

    graph.register(ServiceDescriptor {
        id: "triage".into(),
        name: "Ambient Triage Coprocessor".into(),
        dependencies: vec!["context".into()],
        required_capabilities: 0,
        enabled: true,
        phase: ServicePhase::Standard,
        telemetry_class: TelemetryClass::Core,
    });

    graph.register(ServiceDescriptor {
        id: "telemetry".into(),
        name: "Telemetry Pipeline".into(),
        dependencies: vec![],
        required_capabilities: 0,
        enabled: true,
        phase: ServicePhase::Optional,
        telemetry_class: TelemetryClass::Performance,
    });

    graph.register(ServiceDescriptor {
        id: "skills".into(),
        name: "Skill Runtime".into(),
        dependencies: vec!["tools".into(), "permissions".into()],
        required_capabilities: 0,
        enabled: true,
        phase: ServicePhase::Optional,
        telemetry_class: TelemetryClass::Usage,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_graph_resolution() {
        let mut graph = ServiceGraph::new();
        graph.register(ServiceDescriptor {
            id: "a".into(),
            name: "A".into(),
            dependencies: vec![],
            required_capabilities: 0,
            enabled: true,
            phase: ServicePhase::Core,
            telemetry_class: TelemetryClass::Core,
        });
        graph.register(ServiceDescriptor {
            id: "b".into(),
            name: "B".into(),
            dependencies: vec!["a".into()],
            required_capabilities: 0,
            enabled: true,
            phase: ServicePhase::Standard,
            telemetry_class: TelemetryClass::Core,
        });
        assert!(graph.resolve().is_ok());
        let order = graph.startup_order();
        let a_pos = order.iter().position(|s| s == "a").unwrap();
        let b_pos = order.iter().position(|s| s == "b").unwrap();
        assert!(a_pos < b_pos);
    }

    #[test]
    fn cycle_detected() {
        let mut graph = ServiceGraph::new();
        graph.register(ServiceDescriptor {
            id: "x".into(),
            name: "X".into(),
            dependencies: vec!["y".into()],
            required_capabilities: 0,
            enabled: true,
            phase: ServicePhase::Core,
            telemetry_class: TelemetryClass::Core,
        });
        graph.register(ServiceDescriptor {
            id: "y".into(),
            name: "Y".into(),
            dependencies: vec!["x".into()],
            required_capabilities: 0,
            enabled: true,
            phase: ServicePhase::Core,
            telemetry_class: TelemetryClass::Core,
        });
        assert!(graph.resolve().is_err());
    }

    #[test]
    fn disabled_services_skipped() {
        let mut graph = ServiceGraph::new();
        graph.register(ServiceDescriptor {
            id: "on".into(),
            name: "On".into(),
            dependencies: vec![],
            required_capabilities: 0,
            enabled: true,
            phase: ServicePhase::Core,
            telemetry_class: TelemetryClass::Core,
        });
        graph.register(ServiceDescriptor {
            id: "off".into(),
            name: "Off".into(),
            dependencies: vec![],
            required_capabilities: 0,
            enabled: false,
            phase: ServicePhase::Core,
            telemetry_class: TelemetryClass::Core,
        });
        assert!(graph.resolve().is_ok());
        assert!(!graph.startup_order().contains(&"off".to_string()));
    }
}
