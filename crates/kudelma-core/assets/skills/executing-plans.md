---
name: executing-plans
description: Execute an approved plan step by step, recording verification evidence.
applies_to:
  agents: [lead]
  modes: [build]
priority: required_when_applicable
triggers: [approved_plan]
produces: [implementation_changes, verification_evidence]
---

# Executing Plans

1. Work one plan step at a time; keep the repository runnable between steps.
2. Read the current code before editing; use `index` before large reads.
3. Make the smallest coherent change that satisfies the step.
4. Run the planned check or test after each step.
5. If reality contradicts the plan, stop and revise the plan rather than forcing it.
6. Use `verify` for commands that must become durable evidence.
7. Report completed steps, deviations and remaining work to Lead.
