---
name: technical-brainstorming
description: Explore technical approaches, trade-offs and risks after intent is clear.
applies_to:
  agents: [lead]
  modes: [plan]
priority: optional
triggers: [design_decision, architecture_question, ambiguous_approach]
---

# Technical Brainstorming

1. List the realistic approaches (usually two or three), not every variant.
2. For each: mechanism, affected modules, migration cost, risk and reversibility.
3. State which approach you recommend and why.
4. Identify decisions that need developer approval; do not decide product behavior yourself.
5. Use `explore_codebase` for missing codebase facts; label evidence and inference separately.
6. Close with a small, testable design.
