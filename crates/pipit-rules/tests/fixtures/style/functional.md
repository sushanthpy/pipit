---
kind: preference
description: Prefer functional style in library code
languages:
  - rust
paths:
  - "crates/*/src/**"
---
Prefer functional patterns (iterators, map/filter/fold) over imperative
loops in library code. This improves composability and testability.
