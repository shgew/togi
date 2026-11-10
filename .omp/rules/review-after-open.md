---
agents: main
scope: tool:bash
condition: '\bgh\s+(?:pr\s+create|stack\s+submit)\b'
interruptMode: never
---

For each PR successfully opened by this call or batch, continue `.omp/commands/review-pr.md` on its frozen local commit if review is already running; otherwise start its risk-class review before reporting done. Until R21, documents still need the command's temporary independent review to satisfy robotogi's required `review` check. Use the actual PR numbers, not `all`; dispatch independent PRs together, one coordinator per stack. Use the owner's `gh` identity except for robotogi's record/check. Failed, help-only and dry-run calls open nothing to review. The main session orders merges after coordinators report.
