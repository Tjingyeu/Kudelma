---
name: writing-plans
description: Turn an approved contract into small, ordered implementation steps with verification.
applies_to:
  agents: [lead]
  modes: [plan]
priority: required_when_applicable
triggers: [feature_request, refactor, complex_bug]
produces: [implementation_plan]
---

# Writing Plans

1. Map the files, symbols and boundaries the change touches.
2. Split work into steps that each leave the repository in a valid state.
3. Give every step: goal, files, change, test/verification, rollback risk.
4. Identify dependencies and any steps safe to run in parallel.
5. Define acceptance checks and the exact commands that will produce evidence.
6. Call out unresolved decisions instead of inventing requirements.
7. Keep the plan short enough to execute without re-planning every step.
