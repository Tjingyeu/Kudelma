---
name: verification-before-completion
description: Require machine evidence for builds, tests and checks before claiming completion.
applies_to:
  agents: [lead]
  modes: [review]
priority: required_when_applicable
triggers: [review_gate, completion]
produces: [execution_evidence]
---

# Verification Before Completion

1. Identify the applicable checks: build, unit/integration tests, lint, typecheck,
   acceptance commands and git diff review.
2. Run them through `verify` so command, cwd, exit code, revision and output are recorded.
3. A failing or unverified required check blocks completion.
4. If a check is not applicable, record a concrete reason; do not invent a pass.
5. Summarize evidence IDs and results, including known limitations.
6. Only then mark the task complete.
