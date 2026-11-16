---
name: subagent-driven-development
description: Delegate independent, bounded implementation tasks to Coders and review each report.
applies_to:
  agents: [lead]
  modes: [build]
priority: optional
triggers: [independent_tasks, parallelizable_work, large_feature]
---

# Subagent-Driven Development

1. Delegate only tasks with clear objective, paths, tools and acceptance criteria.
2. Give each Coder a Context Pack, not the full conversation.
3. Do not run parallel Coders on overlapping files unless isolated worktrees are used.
4. Review the Worker Report and actual diff before accepting work.
5. Re-run or independently verify critical checks; a model claim is not evidence.
6. Keep integration and final verification with Lead.
