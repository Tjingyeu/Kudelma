---
name: systematic-debugging
description: Diagnose failures with evidence and hypotheses before editing code.
applies_to:
  agents: [lead]
  modes: [debug]
priority: required_when_applicable
triggers: [bug_report, failing_test, runtime_error]
produces: [root_cause, regression_evidence]
---

# Systematic Debugging

1. Reproduce or precisely characterize the failure.
2. Collect the actual error, logs, versions and relevant state.
3. Form a small set of hypotheses and identify the cheapest discriminating check.
4. Test hypotheses; do not stack speculative edits.
5. Name the root cause with evidence (file/line, log, failing assertion).
6. Add a regression test where feasible.
7. Apply the smallest root-cause fix and verify the regression plus broader checks.
