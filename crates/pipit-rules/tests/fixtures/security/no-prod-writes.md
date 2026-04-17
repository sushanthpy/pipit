---
kind: mandate
description: Never write to production paths
capabilities:
  - FsWrite
  - FsWriteExternal
tier: Validated
forbidden_paths:
  - "production/**"
  - "/etc/**"
---
Never write to production directories or system paths.
Any file modification in production/ requires explicit human approval
via a separate deploy workflow.
