---
description: "Hooks skill with lifecycle declarations"
hooks:
  - event: "post-edit"
    command: "cargo fmt"
  - event: "pre-commit"
    command: "cargo clippy"
---
# Hooks Skill

Skill that declares lifecycle hooks.
