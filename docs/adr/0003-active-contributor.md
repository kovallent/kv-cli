# 0003 — Definition of an active contributor

- **Status:** Accepted
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

## Worked example

This is a test fixture for `docs/adr/0003-active-contributor.md`. It covers one billing cycle in one monitored repository, with seven scenarios. Each scenario tests a different clause of the decision.

- **Cycle:** 2026-09-01T00:00:00Z to 2026-09-30T23:59:59Z
- **Repository:** `acme-corp/finance-marts`, `repository_id = 41`, monitored for the whole cycle
- **Expected billable contributor count: 4**
- **Billable `provider_user_id` values: 8801, 8802, 8804, 8806**

## Two stages, because the ADR applies its rules in two places

ADR 0003 applies some rules when an event **arrives** (clauses 1–3: author not committer, opener counts, bots excluded by `user.type`). It applies the rest when billing **counts** (clause 5: distinct non-null `provider_user_id` over stored `contribution_event` rows). The `contribution_event` table has no column for account type, committer or trailers. So an ingest rule can only be tested by checking which rows ingest writes, not by looking at rows that are already stored.

- **Stage 1, ingest.** `ingest_input` is a list of normalized events in delivery order. This is not GitHub's literal webhook payload; it is the subset of fields ADR 0003 refers to. Running ingest over it must produce exactly the rows in `expected_rows`.
- **Stage 2, billing.** Counting over `expected_rows` must produce `expected`.

A bot, a committer acting only as committer, a co-author named in a trailer, and a reviewer all produce **no row**. They are excluded at ingest, which is the only place the ADR can exclude them. A `provider_user_id: null` row means only *unresolved*. Bots must never be written that way, or they would be counted as unresolved authors.

## Scenario table

| # | Scenario | Clause | Rows written | Billable |
| --- | --- | --- | --- | --- |
| 1 | **Squash merge.** `dana` (8801) pushed 3 commits to a branch. `priya` (8807) squash-merged the PR, so git records priya as committer and dana as author of the squashed commit. Priya does nothing else this cycle. | 1: author, not committer | 4 rows, all 8801 | 8801 |
| 2 | **Co-authored commit.** `mina` (8802) committed with `Co-authored-by: Sam Okafor <sam@acme.com>`. Sam is an invited member whose known email is `sam@acme.com` and whose provider id is 8803, so a trailer *could* be resolved by email. | 1: trailers count only the author | 1 row, 8802 | 8802 |
| 3 | **Bot.** `dependabot[bot]` (49699333, `type: "Bot"`) pushed a commit and opened a PR. | 3: excluded by account type | none | — |
| 4 | **PR opened by a non-committer.** `raj` (8804) opened PR #212, which carries only dana's commit. | 2: the opener counts | 2 rows: 8804 opener, 8801 author | 8804 (8801 already billable) |
| 5 | **Reviewer only.** `lena` (8805) approved PR #212. | 2: review is free | none | — |
| 6 | **Unresolved author.** A commit authored by `noreply@localhost`, with no resolvable provider identity. | 4: stored, never counted | 1 row, null | — |
| 7 | **Control.** `tomas` (8806) pushed an ordinary commit to `main`. | 1 | 1 row, 8806 | 8806 |

One delivery, `d-0004`, arrives twice. Providers redeliver webhooks, and ingest must be idempotent on `delivery_id`.

**Totals:** 13 deliveries in (12 unique, plus the redelivery of `d-0004`), 9 rows written. The rows have four distinct non-null ids, plus one null row.

## The four traps, and what each one catches

Each trap is set so that a specific wrong implementation produces a specific wrong answer.

1. **Billing the committer.** Priya (8807) appears *only* as the committer of the squash commit. An implementation that writes a row for the committer bills her, so the count becomes 5 and 8807 appears.
2. **Resolving co-author trailers.** Sam's email matches a known identity, so an implementation that resolves `Co-authored-by` by email can find him. If it does, 8803 appears and the count becomes 5. Under the ADR the trailer is free text and is never resolved.
3. **Storing reviews, or excluding bots by name.** Lena (8805) billed means reviews were stored. Writing the bot's events produces either 49699333 in the count (count 5) or an extra unresolved row (`unresolved_event_count == 3` rather than 1). The bot's `login` deliberately ends in `[bot]` *and* its `type` is `"Bot"`, so a name rule and the type rule agree here. A separate assertion requires exclusion to key on `type`.
4. **Not being idempotent.** Processing the duplicate `d-0004` twice writes 10 rows, not 9. The billable count is unaffected because it counts distinct ids, which is exactly why the trap asserts the row count rather than the billable count.

## JSON fixture

```json
{
  "fixture": "adr-0003-worked-example",
  "version": 2,
  "cycle": { "start": "2026-09-01T00:00:00Z", "end": "2026-09-30T23:59:59Z" },
  "repository": { "repository_id": 41, "full_name": "acme-corp/finance-marts" },

  "known_identities": [
    { "provider_user_id": 8803, "login": "sokafor", "email": "sam@acme.com",
      "member_source": "invited",
      "note": "pre-existing invited member; present so a trailer COULD be resolved by email" }
  ],

  "ingest_input": [
    { "delivery_id": "d-0001", "scenario": 1, "event": "commit", "occurred_at": "2026-09-03T09:14:22Z",
      "author":    { "id": 8801, "login": "dana",  "type": "User" },
      "committer": { "id": 8801, "login": "dana",  "type": "User" }, "trailers": [] },
    { "delivery_id": "d-0002", "scenario": 1, "event": "commit", "occurred_at": "2026-09-03T09:41:07Z",
      "author":    { "id": 8801, "login": "dana",  "type": "User" },
      "committer": { "id": 8801, "login": "dana",  "type": "User" }, "trailers": [] },
    { "delivery_id": "d-0003", "scenario": 1, "event": "commit", "occurred_at": "2026-09-03T10:05:51Z",
      "author":    { "id": 8801, "login": "dana",  "type": "User" },
      "committer": { "id": 8801, "login": "dana",  "type": "User" }, "trailers": [] },
    { "delivery_id": "d-0004", "scenario": 1, "event": "commit", "occurred_at": "2026-09-03T15:02:18Z",
      "note": "squash commit on main: author dana, committer priya",
      "author":    { "id": 8801, "login": "dana",  "type": "User" },
      "committer": { "id": 8807, "login": "priya", "type": "User" }, "trailers": [] },

    { "delivery_id": "d-0005", "scenario": 2, "event": "commit", "occurred_at": "2026-09-08T14:02:55Z",
      "author":    { "id": 8802, "login": "mina",  "type": "User" },
      "committer": { "id": 8802, "login": "mina",  "type": "User" },
      "trailers":  ["Co-authored-by: Sam Okafor <sam@acme.com>"] },

    { "delivery_id": "d-0006", "scenario": 3, "event": "commit", "occurred_at": "2026-09-11T02:00:31Z",
      "author":    { "id": 49699333, "login": "dependabot[bot]", "type": "Bot" },
      "committer": { "id": 49699333, "login": "dependabot[bot]", "type": "Bot" }, "trailers": [] },
    { "delivery_id": "d-0007", "scenario": 3, "event": "pull_request_opened", "occurred_at": "2026-09-11T02:00:44Z",
      "sender":    { "id": 49699333, "login": "dependabot[bot]", "type": "Bot" } },

    { "delivery_id": "d-0008", "scenario": 4, "event": "commit", "occurred_at": "2026-09-17T10:58:13Z",
      "author":    { "id": 8801, "login": "dana",  "type": "User" },
      "committer": { "id": 8801, "login": "dana",  "type": "User" }, "trailers": [] },
    { "delivery_id": "d-0009", "scenario": 4, "event": "pull_request_opened", "occurred_at": "2026-09-17T11:20:09Z",
      "note": "PR #212, carrying only d-0008",
      "sender":    { "id": 8804, "login": "raj",   "type": "User" } },

    { "delivery_id": "d-0010", "scenario": 5, "event": "pull_request_review", "occurred_at": "2026-09-17T13:44:00Z",
      "state": "approved",
      "sender":    { "id": 8805, "login": "lena",  "type": "User" } },

    { "delivery_id": "d-0011", "scenario": 6, "event": "commit", "occurred_at": "2026-09-22T16:31:40Z",
      "author":    { "id": null, "login": null, "type": null, "email": "noreply@localhost" },
      "committer": { "id": null, "login": null, "type": null, "email": "noreply@localhost" }, "trailers": [] },

    { "delivery_id": "d-0012", "scenario": 7, "event": "commit", "occurred_at": "2026-09-28T08:12:04Z",
      "author":    { "id": 8806, "login": "tomas", "type": "User" },
      "committer": { "id": 8806, "login": "tomas", "type": "User" }, "trailers": [] },

    { "delivery_id": "d-0004", "scenario": 1, "event": "commit", "occurred_at": "2026-09-03T15:02:18Z",
      "note": "provider redelivery of d-0004; must not write a second row",
      "author":    { "id": 8801, "login": "dana",  "type": "User" },
      "committer": { "id": 8807, "login": "priya", "type": "User" }, "trailers": [] }
  ],

  "expected_rows": [
    { "delivery_id": "d-0001", "repository_id": 41, "provider_user_id": 8801, "event_type": "commit",              "occurred_at": "2026-09-03T09:14:22Z" },
    { "delivery_id": "d-0002", "repository_id": 41, "provider_user_id": 8801, "event_type": "commit",              "occurred_at": "2026-09-03T09:41:07Z" },
    { "delivery_id": "d-0003", "repository_id": 41, "provider_user_id": 8801, "event_type": "commit",              "occurred_at": "2026-09-03T10:05:51Z" },
    { "delivery_id": "d-0004", "repository_id": 41, "provider_user_id": 8801, "event_type": "commit",              "occurred_at": "2026-09-03T15:02:18Z" },
    { "delivery_id": "d-0005", "repository_id": 41, "provider_user_id": 8802, "event_type": "commit",              "occurred_at": "2026-09-08T14:02:55Z" },
    { "delivery_id": "d-0008", "repository_id": 41, "provider_user_id": 8801, "event_type": "commit",              "occurred_at": "2026-09-17T10:58:13Z" },
    { "delivery_id": "d-0009", "repository_id": 41, "provider_user_id": 8804, "event_type": "pull_request_opened", "occurred_at": "2026-09-17T11:20:09Z" },
    { "delivery_id": "d-0011", "repository_id": 41, "provider_user_id": null, "event_type": "commit",              "occurred_at": "2026-09-22T16:31:40Z" },
    { "delivery_id": "d-0012", "repository_id": 41, "provider_user_id": 8806, "event_type": "commit",              "occurred_at": "2026-09-28T08:12:04Z" }
  ],

  "expected": {
    "rows_written": 9,
    "billable_count": 4,
    "billable_provider_user_ids": [8801, 8802, 8804, 8806],
    "unresolved_event_count": 1,
    "not_billed": [
      { "provider_user_id": 8807,     "reason": "committer_only",                   "excluded_at": "ingest" },
      { "provider_user_id": 8803,     "reason": "co_author_trailer_not_resolved",   "excluded_at": "ingest" },
      { "provider_user_id": 49699333, "reason": "account_type_bot",                 "excluded_at": "ingest" },
      { "provider_user_id": 8805,     "reason": "review_only",                      "excluded_at": "ingest" }
    ]
  },

  "assertions": [
    "stage 1: ingest over ingest_input writes exactly expected_rows (set equality on delivery_id + provider_user_id + event_type)",
    "stage 1: rows_written == 9 — the redelivered d-0004 writes nothing",
    "stage 1: no row has provider_user_id in {8803, 8805, 8807, 49699333}",
    "stage 1: bot exclusion keys on author/sender type == 'Bot', never on login; a test that sets login to 'dependabot' (no [bot] suffix) with type 'Bot' must still write no row",
    "stage 1: no Co-authored-by trailer is resolved, even when its email matches known_identities",
    "stage 2: billable_count == count(DISTINCT provider_user_id) over expected_rows WHERE provider_user_id IS NOT NULL == 4",
    "stage 2: set(billable) == {8801, 8802, 8804, 8806}",
    "stage 2: 8801 has 5 rows and counts once",
    "stage 2: unresolved_event_count == 1",
    "ADR 0004: each of 8801, 8802, 8804, 8806 has an organization_member row with source = 'contribution' and NO workspace_role",
    "ADR 0004: 8803 keeps its existing member row with source = 'invited'; ingest creates no second member row and grants no role"
  ]
}
```

## Edge cases this fixture does not cover

Each needs its own fixture before Phase 5 ships, and each needs a decision the ADR does not make:

- **A commit authored inside the cycle but delivered after it closes.** Its `occurred_at` places it in the earlier cycle, so an invoice that was already issued becomes wrong after the fact. Whether that reopens the invoice or carries the charge forward is a billing-policy decision, not an ADR 0003 one.
- **A repository that stops being monitored mid-cycle.** Its events stay stored. Whether they remain billable is not tested here.
- **A contributor active in two monitored repositories in one workspace.** They should count once for the workspace. Assert it explicitly, because grouping by repository is the easy mistake.
