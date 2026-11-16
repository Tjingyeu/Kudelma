---
name: using-git-worktrees
description: Isolate risky or parallel work in a Git worktree when available.
applies_to:
  agents: [lead]
  modes: [build]
priority: optional
triggers: [parallel_work, risky_refactor, isolation_needed]
---

# Using Git Worktrees

1. Check that the repository is a clean Git checkout and worktrees are available.
2. Create a short-lived worktree/branch for the isolated task.
3. Keep ownership of overlapping paths with one worker only.
4. Record the worktree path in the Worker Task and evidence.
5. Integrate deliberately and run final verification in the main checkout.
6. Remove temporary worktrees only after the work is accepted.
