## Summary

<!-- Briefly say what changes, using the smallest useful view: prose, pseudocode, a call tree or a diff. For behavior, link the owner-approved docs/model.md diff; distinguish design from shipped behavior. Direct owner requests need no separate issue; reference the tracking issue for a large change. -->

## Evidence

- **Before:** <!-- screenshot, output or failing test run -->
  **After:** <!-- screenshot, output or passing test run -->

<!-- List exact checks and their results, not planned runs. Code: failing-before/passing-after proof or a scoped reason it is impractical. Documents: quality/link check and CI; note the temporary independent-review requirement until R21. Tuning behavior: link the target-desktop result still owed before the next design; only the owner runs togi there. Coverage is optional evidence except where the current record renderer requires it until R21. -->

## Merge Danger

**Door:** <!-- one-way or two-way -->

<!-- Optional: why. A two-way door is cheap to roll back; a destructive or hard-to-reverse change, such as a journal schema or ruleset bump, is one-way. -->

**Blast Radius:** <!-- one word -->

<!-- Name affected behavior or safety invariants. If existing journals become unreadable or recorded decisions unrepeatable, say which SCHEMA or RULESET changed and that old journals are refused. Before 1.0, no breaking title/label or new changelog fragment. Map every removed/narrowed test assertion to its surviving proof or approved retirement. -->

<!-- Closes #N only when all Acceptance is met; otherwise Refs #N. Omit for a direct request without an issue. -->
