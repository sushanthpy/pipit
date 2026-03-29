//! Architecture Genome — Task 10.1
//!
//! Typed directed graph G = (V, E, τ_v, τ_e):
//! - Vertices: services with type annotations (Stateless, Stateful, Database, etc.)
//! - Edges: communication channels (Sync_RPC, Async_Message, Event_Stream, Shared_DB)
//! Mutation operators: split, merge, retype, add/remove edge.
//! Genome size: O(|V| + |E|), each mutation O(|V|).

use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Architecture genome: a typed directed graph of services.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchGenome {
    pub services: Vec<Service>,
    pub channels: Vec<Channel>,
    pub metadata: GenomeMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    pub id: usize,
    pub name: String,
    pub service_type: ServiceType,
    /// Estimated monthly cost ($).
    pub cost_estimate: f64,
    /// Estimated processing latency (ms).
    pub latency_estimate: f64,
    /// Estimated reliability (0-1).
    pub reliability: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServiceType {
    Stateless,
    Stateful,
    Database,
    Cache,
    Queue,
    Gateway,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub from: usize,
    pub to: usize,
    pub channel_type: ChannelType,
    pub reliability: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChannelType {
    SyncRpc,
    AsyncMessage,
    EventStream,
    SharedDb,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenomeMetadata {
    pub generation: u32,
    pub parent_ids: Vec<String>,
    pub fitness_scores: Vec<f64>,
}

/// Mutation operators for architecture genomes.
#[derive(Debug, Clone, Copy)]
pub enum Mutation {
    SplitService,
    MergeServices,
    RetypeService,
    RetypeChannel,
    AddChannel,
    RemoveChannel,
}

impl ArchGenome {
    /// Create a simple monolith genome.
    pub fn monolith(name: &str) -> Self {
        Self {
            services: vec![Service {
                id: 0, name: name.to_string(),
                service_type: ServiceType::Stateful,
                cost_estimate: 50.0, latency_estimate: 5.0, reliability: 0.99,
            }],
            channels: vec![],
            metadata: GenomeMetadata::default(),
        }
    }

    /// Apply a random mutation. O(|V|).
    pub fn mutate(&mut self, rng: &mut impl Rng) {
        let mutation = match rng.gen_range(0..6) {
            0 => self.split_service(rng),
            1 => self.merge_services(rng),
            2 => self.retype_service(rng),
            3 => self.retype_channel(rng),
            4 => self.add_channel(rng),
            _ => self.remove_channel(rng),
        };
    }

    pub fn split_service(&mut self, rng: &mut impl Rng) {
        if self.services.is_empty() { return; }
        let idx = rng.gen_range(0..self.services.len());
        let original = &self.services[idx];
        let new_id = self.services.len();

        let svc1 = Service {
            id: original.id,
            name: format!("{}-a", original.name),
            service_type: ServiceType::Stateless,
            cost_estimate: original.cost_estimate * 0.6,
            latency_estimate: original.latency_estimate * 0.8,
            reliability: original.reliability,
        };
        let svc2 = Service {
            id: new_id,
            name: format!("{}-b", original.name),
            service_type: original.service_type,
            cost_estimate: original.cost_estimate * 0.5,
            latency_estimate: original.latency_estimate * 0.5,
            reliability: original.reliability,
        };

        self.services[idx] = svc1;
        self.services.push(svc2);

        // Add communication channel between split services
        self.channels.push(Channel {
            from: idx, to: new_id,
            channel_type: ChannelType::SyncRpc,
            reliability: 0.999,
        });
    }

    fn merge_services(&mut self, rng: &mut impl Rng) {
        if self.services.len() < 2 { return; }
        let a = rng.gen_range(0..self.services.len());
        let b = (a + 1) % self.services.len();
        let merged_name = format!("{}-{}", self.services[a].name, self.services[b].name);
        let merged_cost = self.services[a].cost_estimate + self.services[b].cost_estimate * 0.7;
        self.services[a].name = merged_name;
        self.services[a].cost_estimate = merged_cost;
        let remove_id = self.services[b].id;
        self.services.remove(b);
        self.channels.retain(|c| c.from != remove_id && c.to != remove_id);
    }

    fn retype_service(&mut self, rng: &mut impl Rng) {
        if self.services.is_empty() { return; }
        let idx = rng.gen_range(0..self.services.len());
        let types = [ServiceType::Stateless, ServiceType::Stateful, ServiceType::Database,
                     ServiceType::Cache, ServiceType::Queue, ServiceType::Gateway];
        self.services[idx].service_type = types[rng.gen_range(0..types.len())];
    }

    fn retype_channel(&mut self, rng: &mut impl Rng) {
        if self.channels.is_empty() { return; }
        let idx = rng.gen_range(0..self.channels.len());
        let types = [ChannelType::SyncRpc, ChannelType::AsyncMessage,
                     ChannelType::EventStream, ChannelType::SharedDb];
        self.channels[idx].channel_type = types[rng.gen_range(0..types.len())];
    }

    fn add_channel(&mut self, rng: &mut impl Rng) {
        if self.services.len() < 2 { return; }
        let from = rng.gen_range(0..self.services.len());
        let to = (from + rng.gen_range(1..self.services.len())) % self.services.len();
        self.channels.push(Channel {
            from, to,
            channel_type: ChannelType::AsyncMessage,
            reliability: 0.999,
        });
    }

    fn remove_channel(&mut self, rng: &mut impl Rng) {
        if self.channels.is_empty() { return; }
        let idx = rng.gen_range(0..self.channels.len());
        self.channels.remove(idx);
    }

    /// Crossover: vertex set from self, edge topology from other.
    pub fn crossover(&self, other: &Self, rng: &mut impl Rng) -> Self {
        let mut child = self.clone();
        child.channels = other.channels.iter()
            .filter(|c| c.from < child.services.len() && c.to < child.services.len())
            .cloned()
            .collect();
        child.metadata.generation = self.metadata.generation.max(other.metadata.generation) + 1;
        child
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monolith_creation() {
        let g = ArchGenome::monolith("my-app");
        assert_eq!(g.services.len(), 1);
        assert!(g.channels.is_empty());
    }

    #[test]
    fn test_mutation_split() {
        let mut g = ArchGenome::monolith("app");
        let mut rng = rand::thread_rng();
        g.split_service(&mut rng);
        assert_eq!(g.services.len(), 2, "Split should create 2 services");
        assert_eq!(g.channels.len(), 1, "Split should add a channel");
    }

    #[test]
    fn test_evolution_produces_variety() {
        let mut rng = rand::thread_rng();
        let mut genomes: Vec<ArchGenome> = (0..10).map(|_| ArchGenome::monolith("app")).collect();

        // 5 generations of mutation
        for _ in 0..5 {
            for g in &mut genomes {
                g.mutate(&mut rng);
            }
        }

        let service_counts: Vec<usize> = genomes.iter().map(|g| g.services.len()).collect();
        let unique_counts: std::collections::HashSet<_> = service_counts.iter().collect();
        assert!(unique_counts.len() > 1, "Evolution should produce variety: {:?}", service_counts);
    }
}
