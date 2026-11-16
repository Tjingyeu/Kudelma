---
name: test-driven-development
description: Use RED-GREEN-REFACTOR for behavior changes and testable bug fixes.
applies_to:
  agents: [lead, coder]
  modes: [build, debug]
priority: required_when_applicable
triggers: [behavior_change, regression_fix, business_logic]
produces: [test_evidence, implementation_changes]
---

# Test-Driven Development

1. Write or identify a test that fails for the expected reason. Run it and record the
   failing evidence with `verify` (or report honestly that no harness exists).
2. Make the smallest implementation change that turns it green.
3. Run the focused test, then the relevant broader suite.
4. Refactor only while tests stay green.
5. For bug fixes, add a regression test when feasible; if not, record why.
6. Never claim RED or GREEN without command evidence. UI/visual work may use a
   runtime or visual check instead, with the reason recorded.
