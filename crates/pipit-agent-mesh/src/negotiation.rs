//! Schema Negotiation Protocol — Task 2.2
//!
//! Three-phase negotiation: propose → counter-propose → commit.
//! Schema intersection via lattice meet operation on JSON Schema types.
//! Convergence guaranteed in ≤3 rounds (meet is idempotent).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A JSON Schema-like schema proposal for inter-agent communication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SchemaProposal {
    pub properties: BTreeMap<String, SchemaType>,
    pub required: BTreeSet<String>,
}

/// Simplified JSON Schema type system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SchemaType {
    String,
    Integer,
    Number,
    Boolean,
    Array(Box<SchemaType>),
    Object(BTreeMap<String, SchemaType>),
    Any,
}

/// Result of schema negotiation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NegotiationResult {
    Agreed(SchemaProposal),
    Rejected { reason: String },
}

/// Three-phase negotiation protocol.
pub struct NegotiationProtocol;

impl NegotiationProtocol {
    /// Compute the meet (intersection) of two schemas.
    /// Retains only properties present in both with the strictest type.
    /// O(|P₁| + |P₂|). Idempotent: (S₁ ∧ S₂) ∧ S₂ = S₁ ∧ S₂.
    pub fn schema_meet(s1: &SchemaProposal, s2: &SchemaProposal) -> SchemaProposal {
        let mut properties = BTreeMap::new();
        let mut required = BTreeSet::new();

        // Retain only properties in both schemas
        for (key, type1) in &s1.properties {
            if let Some(type2) = s2.properties.get(key) {
                let merged_type = Self::type_meet(type1, type2);
                properties.insert(key.clone(), merged_type);

                // Required only if required in both
                if s1.required.contains(key) && s2.required.contains(key) {
                    required.insert(key.clone());
                }
            }
        }

        SchemaProposal {
            properties,
            required,
        }
    }

    /// Type meet: strictest type that satisfies both constraints.
    /// integer ∧ number = integer (stricter)
    /// string ∧ any = string
    fn type_meet(t1: &SchemaType, t2: &SchemaType) -> SchemaType {
        match (t1, t2) {
            // Same type → keep it
            (a, b) if a == b => a.clone(),
            // Any meets anything → the other type
            (SchemaType::Any, other) | (other, SchemaType::Any) => other.clone(),
            // Integer is stricter than Number
            (SchemaType::Integer, SchemaType::Number)
            | (SchemaType::Number, SchemaType::Integer) => SchemaType::Integer,
            // Array: meet the element types
            (SchemaType::Array(a), SchemaType::Array(b)) => {
                SchemaType::Array(Box::new(Self::type_meet(a, b)))
            }
            // Object: recursive meet
            (SchemaType::Object(a), SchemaType::Object(b)) => {
                let mut merged = BTreeMap::new();
                for (key, type_a) in a {
                    if let Some(type_b) = b.get(key) {
                        merged.insert(key.clone(), Self::type_meet(type_a, type_b));
                    }
                }
                SchemaType::Object(merged)
            }
            // Incompatible types → String (safest common representation)
            _ => SchemaType::String,
        }
    }

    /// Execute the three-phase negotiation.
    /// Round 1: proposer sends initial schema
    /// Round 2: responder computes meet with its ideal schema
    /// Round 3: proposer verifies (meet is idempotent, so this always agrees)
    pub fn negotiate(
        proposer_schema: &SchemaProposal,
        responder_ideal: &SchemaProposal,
    ) -> NegotiationResult {
        // Round 2: responder computes meet
        let agreed = Self::schema_meet(proposer_schema, responder_ideal);

        // Round 3: verify non-empty
        if agreed.properties.is_empty() {
            return NegotiationResult::Rejected {
                reason: "No common properties between schemas".to_string(),
            };
        }

        // Verify idempotency: (S₁ ∧ S₂) ∧ S₂ = S₁ ∧ S₂
        let verify = Self::schema_meet(&agreed, responder_ideal);
        debug_assert_eq!(agreed, verify, "Meet must be idempotent");

        NegotiationResult::Agreed(agreed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_meet_retains_common_properties() {
        let s1 = SchemaProposal {
            properties: BTreeMap::from([
                ("name".into(), SchemaType::String),
                ("age".into(), SchemaType::Integer),
                ("email".into(), SchemaType::String),
            ]),
            required: BTreeSet::from(["name".into(), "age".into()]),
        };

        let s2 = SchemaProposal {
            properties: BTreeMap::from([
                ("name".into(), SchemaType::String),
                ("age".into(), SchemaType::Number), // Number → Integer via meet
                ("address".into(), SchemaType::String),
            ]),
            required: BTreeSet::from(["name".into()]),
        };

        let meet = NegotiationProtocol::schema_meet(&s1, &s2);
        assert_eq!(meet.properties.len(), 2, "Only name + age in common");
        assert_eq!(meet.properties["age"], SchemaType::Integer, "Integer ∧ Number = Integer");
        assert_eq!(meet.required.len(), 1, "Only 'name' required in both");
        assert!(meet.required.contains("name"));
    }

    #[test]
    fn test_negotiation_succeeds() {
        let proposer = SchemaProposal {
            properties: BTreeMap::from([
                ("task_id".into(), SchemaType::String),
                ("result".into(), SchemaType::Any),
            ]),
            required: BTreeSet::from(["task_id".into()]),
        };

        let responder = SchemaProposal {
            properties: BTreeMap::from([
                ("task_id".into(), SchemaType::String),
                ("result".into(), SchemaType::String),
                ("duration_ms".into(), SchemaType::Integer),
            ]),
            required: BTreeSet::from(["task_id".into(), "result".into()]),
        };

        match NegotiationProtocol::negotiate(&proposer, &responder) {
            NegotiationResult::Agreed(schema) => {
                assert_eq!(schema.properties.len(), 2);
                assert_eq!(schema.properties["result"], SchemaType::String, "Any ∧ String = String");
            }
            NegotiationResult::Rejected { reason } => panic!("Should agree: {}", reason),
        }
    }

    #[test]
    fn test_meet_is_idempotent() {
        let s1 = SchemaProposal {
            properties: BTreeMap::from([("x".into(), SchemaType::Integer)]),
            required: BTreeSet::from(["x".into()]),
        };
        let s2 = SchemaProposal {
            properties: BTreeMap::from([
                ("x".into(), SchemaType::Number),
                ("y".into(), SchemaType::String),
            ]),
            required: BTreeSet::new(),
        };

        let meet1 = NegotiationProtocol::schema_meet(&s1, &s2);
        let meet2 = NegotiationProtocol::schema_meet(&meet1, &s2);
        assert_eq!(meet1, meet2, "Meet must be idempotent: (S1∧S2)∧S2 = S1∧S2");
    }
}
