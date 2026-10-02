# Pull requests under the owner's account

[ADR 0025](0025-recorded-agent-review.md) made the GitHub App `robotogi` the agents' identity for every GitHub action: opening pull requests, pushing, commenting, replying to and resolving threads, posting check runs and merging when the owner asks. The owner wants agent pull requests and the conversation on them under the owner's account, and the App limited to the reviews it certifies.

## Decision

Agents act on GitHub with the owner's account through `gh`: they open pull requests, submit and sync stacks, push, comment, reply to and resolve review threads, file issues and perform merges the owner asked for.

robotogi only posts reviews: the review record comment and the `review` check run, through `just bot`. The `main` ruleset still requires `review` from robotogi and resolved review threads, alongside CI.

`.omp/rules/review-after-open.md` watches `gh pr create` and `gh stack submit`; `just bot` no longer opens pull requests.

## Considered Options

- **Keep the App as the agents' identity for every action:** rejected. The owner wants agent pull requests under the owner's account.

## Consequences

Authorship and review come from separate identities: the pull request, its commits and the replies to findings are the owner's; the record and check that gate the merge are robotogi's. The owner again cannot approve agent pull requests with a GitHub review, as before the App; the ruleset requires the `review` check, not an approval. The App no longer pushes, opens pull requests or merges, so it no longer uses its write access to contents.
