---
agents: "!reviewer"
scope: tool:bash
condition: '\b(?:just\s+bot\s+(?:pr\s+create|stack\s+submit)|gh\s+(?:pr\s+create|stack\s+submit))\b'
interruptMode: never
---

Opening a pull request starts its review. Read the completed command's result and any other PR-opening results in the same tool batch. For every pull request they successfully opened, run the flow in `.omp/commands/review-pr.md` before reporting done, using those PR numbers rather than `all`. Review multiple opened PRs in parallel, with one coordinator for each stack. Use `just bot` for GitHub actions. If a command failed or only showed help or a dry run, review only PRs it actually opened. Never merge.
