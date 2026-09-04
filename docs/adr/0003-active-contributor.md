# 0003 — Definition of an active contributor

- **Status:** Proposed
- **Date:** 2026-09-04
- **Deciders:** platform
- **Supersedes:** none
- **Blocks:** Phase 2 (webhook ingest), Phase 5 (billing)
- **Extended by:** ADR 0004 (membership and roles) — a billable contributor is provisioned as an
  organization member on first billable event, with no workspace role

## Context

The pricing model bills per active contributor: "any developer who opens a pull request or makes
a commit within a monitored repository during the billing cycle. Passive code reviewers or
observers are free."

That sentence is a financial interface, and it is ambiguous in four places that only become
visible once code is reading webhooks. Each ambiguity, left unresolved, produces an invoice a
customer can dispute and we cannot defend from the data — so the definition has to be settled
before any webhook-handling code exists, not after.

This ADR is deliberately narrow: it defines who counts. It does not decide prices, proration, or
what happens when a customer exceeds their tier.

## Decision

**1. Commit author, not committer.** Git records both, and they diverge on rebases, squash
merges, and cherry-picks — where the committer is whoever ran the merge. Billing the committer
would charge a release manager for everyone else's work and would make one person's merge queue
look like ten active contributors. The author is the person who wrote the change.

Co-authored commits (`Co-authored-by:` trailers) count **only** the author. Pair-programming
trailers are informational and are not resolvable to provider identities reliably enough to bill.

**2. Pull request openers count; reviewers and commenters do not.** Opening a pull request is
what triggers the CI gate and therefore consumes the product. Review is explicitly free per the
published model, and that includes approving, requesting changes, and commenting.

A pull request opened by one person carrying another person's commits makes **both** active — the
author for the commit, the opener for the pull request.

**3. Bots are excluded by account type, never by name.** The exclusion test is
`user.type == "Bot"` on the provider's user object. Matching names ending in `[bot]`, or a
maintained denylist of names like `dependabot` or `renovate`, fails in both directions: it misses
self-hosted automation and it can exclude a human whose username happens to match. Where a
provider does not expose an account type, the identity is treated as human — over-counting is a
conversation, under-counting is revenue we cannot recover.

**4. Deduplicate on the provider's numeric user id.** Never on email. Git email addresses vary per
machine and per commit, users set `noreply` addresses, and monorepo contributors frequently commit
under two identities. The numeric id is stable across username changes and email changes. A commit
whose author cannot be resolved to a provider identity is recorded as unattributed and does not
count — the same stance as ADR 0002 takes on unattributable runs.

**5. Contributor counts are derived, never accumulated.** Store the raw webhook events —
`{provider_user_id, repository_id, event_type, occurred_at}` — and compute the active set at
billing time by querying that table. Never maintain a running counter.

This is the decision most likely to be implemented the other way for performance reasons, and the
reason not to is recoverability: an incrementing counter cannot be recomputed after a bug, a
replay, or a definition change. A derived count can be recomputed for any past period at any time,
which is exactly what an invoice dispute requires. The events table is small — one row per commit
and pull request, not per audit run.

### Schema consequence

```
contribution_event
  id
  repository_id        NOT NULL
  provider_user_id     NULL       -- null = unresolved, never counted
  event_type                      -- 'commit' | 'pull_request_opened'
  occurred_at          NOT NULL   -- authored/opened time, not received time
  delivery_id          UNIQUE     -- provider webhook id, for idempotent replay
```

`occurred_at` is the authored or opened time, not when the webhook arrived. A delayed or replayed
delivery must land in the cycle where the work happened. `delivery_id` makes redelivery idempotent
— providers redeliver, and a duplicated commit event would inflate nothing today but would corrupt
any future per-event metric.

## Consequences

**A billing period is a query, and the query is auditable.** "Why is this invoice 12 contributors?"
is answered by listing the twelve identities and the events that made each active — which is the
only answer that ends the conversation.

**Definition changes are retroactive by construction.** If reviewers ever become billable, the
historical numbers can be recomputed rather than estimated.

**Webhook loss is a correctness risk, not just a monitoring one.** Because counts are derived,
a missed delivery undercounts permanently. Phase 2 needs a reconciliation job that walks the
provider's API for the period and backfills anything missing.

**Unresolved identities need visibility.** A repository whose commits arrive without resolvable
authors will silently bill zero. Surface an unresolved-event count per repository in settings.

**A resolved identity provisions a member.** Per ADR 0004, the first billable event from an
unrecognised `provider_user_id` creates an organization member with `source = 'contribution'` and
no workspace role, so every charge is attributable to a named row. Counting stays derived from
this table; the member record is an identity, not a counter.

## Rejected alternatives

**Bill per seat invited to the workspace.** Trivial to compute and contradicts the published
model, which is explicitly usage-based. It also punishes exactly the adoption pattern we want —
inviting a whole team to read dashboards.

**Bill per audit run.** Aligns cost with compute, and it directly discourages running the tool,
which is the opposite of the product's purpose. Local runs are free forever for the same reason.

**Count anyone who appears in git history for the period.** Includes reviewers via merge commits
and revives the committer-versus-author problem this ADR exists to settle.

**Maintain a running counter per cycle.** Cheaper at billing time, unrecoverable after any bug.
Rejected on the recomputability argument above.
