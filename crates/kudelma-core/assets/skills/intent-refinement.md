---
name: intent-refinement
description: Clarify the developer's intent and produce an approved Task Contract before any implementation.
applies_to:
  agents: [agency]
  modes: [build]
priority: required_when_applicable
triggers: [feature_request, bug_report, change_request, planning_request]
produces: [task_contract]
---

# Intent Refinement

1. Restate the requested outcome in one or two sentences.
2. Identify only the ambiguities that would materially change scope, behavior, compatibility or acceptance.
3. Ask at most one focused clarification question per turn. Offer a recommended default when useful.
4. Record low-risk assumptions instead of blocking.
5. Stop when the contract is actionable; do not interrogate the developer.
6. Use `submit_contract` with: summary, goals, requirements, non-goals, constraints,
   assumptions, unresolved questions, acceptance criteria, complexity and intent kind.
7. Never mark the work approved while a material question is unresolved.
