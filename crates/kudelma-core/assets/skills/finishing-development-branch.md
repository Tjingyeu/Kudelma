---
name: finishing-development-branch
description: Decide explicitly whether to merge, retain or clean up a development branch.
applies_to:
  agents: [lead]
  modes: [review]
priority: optional
triggers: [review_complete, branch_cleanup]
---

# Finishing a Development Branch

1. Confirm verification evidence is green for the final diff.
2. Review the diff scope and ensure no unrelated changes are included.
3. Decide: merge, keep the branch, or discard; state the reason.
4. Do not delete worktrees or branches with uncommitted/unmerged user changes.
5. Record the decision and any follow-up work in the completion report.
