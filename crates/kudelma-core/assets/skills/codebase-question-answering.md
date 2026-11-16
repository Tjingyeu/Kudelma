---
name: codebase-question-answering
description: Answer questions about existing code through bounded deterministic read-only exploration, without activating Lead.
applies_to:
  agents: [agency]
  modes: [build]
priority: required_when_applicable
triggers: [code_question, architecture_question]
produces: [exploration_report]
---

# Codebase Question Answering

1. Use `explore_codebase` for codebase questions; do not guess file contents.
2. Provide a specific objective, scope hints and optional symbols. The capability is
   deterministic and has no model of its own: you interpret the returned evidence.
3. Prefer `file:symbol:line` citations over general statements.
4. Separate observed facts from inference; state what is unknown or outside the search scope.
5. Answer the developer directly. Only create a Lead task if they request a
   design decision, plan, or code change.
6. Never edit project files or run arbitrary commands to answer a question.
