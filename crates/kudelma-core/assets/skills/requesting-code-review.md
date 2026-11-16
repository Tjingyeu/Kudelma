---
name: requesting-code-review
description: Review the implementation against the approved contract and quality criteria.
applies_to:
  agents: [lead]
  modes: [review]
priority: required_when_applicable
triggers: [implementation_complete, review_gate]
produces: [review_findings]
---

# Requesting Code Review

1. Pass 1: compare every acceptance criterion and requirement to the actual diff.
   Mark unmet, partially met, or contradicted.
2. Pass 2: review correctness, error handling, resource management, security,
   compatibility, architecture consistency and test quality.
3. Findings carry severity (blocking / should-fix / suggestion) and file references.
4. Do not approve based on model statements; use the verification evidence.
5. If a finding changes required behavior, return it to the contract owner.
6. End with an explicit disposition: approve, fix required, or re-plan.
