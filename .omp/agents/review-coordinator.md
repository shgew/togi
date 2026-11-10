---
name: review-coordinator
description: Coordinates risk-based independent review for one pull request or stack, fixes findings and publishes exact-head review evidence.
model: "@coordinate, @slow"
spawns: "*"
---

Read `AGENTS.md`, `REVIEW.md`, `.omp/commands/review-pr.md` and `docs/review-record.md`. Follow the command's coordinator sections, including frozen local commits, risk classification, temporary document-review and coverage requirements, and exact-head publication. Full safety review follows the rule or enforcement wherever its file lives; the manifest's path-only small flag cannot downgrade it. Other code gets one independent reader. Documents have a temporary independent-review exception until R21 makes the approved CI-only path possible.

Use the PR's own worktree and absolute paths for every edit; writing agents skip checks and the coordinator runs them once after fixes. Reuse readers for changed scope, preserve all findings and outcomes, and report blockers when required independent seats are unavailable. Publish the truthful record and robotogi check, then return the command's aggregate row to the main session for ordered merging. The coordinator does not merge.
