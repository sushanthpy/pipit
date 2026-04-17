---
kind: procedure
description: Run tests before committing
capabilities:
  - Verify
  - ProcessExec
required_sequence:
  - test
  - commit
---
Always run `cargo test` (or the project's equivalent test command)
before making a git commit. This ensures regressions are caught early.
