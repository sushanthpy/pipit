---
kind: invariant
description: No circular dependencies between crates
---
The workspace must maintain a DAG dependency structure.
No crate may depend on another crate that directly or transitively
depends back on it.
