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

This is a test fixture for `docs/adr/0003-active-contributor.md`. It covers one billing cycle in one monitored repository, with nine scenarios. Each scenario tests a different clause of the decision.

This is fixture version 5. It follows the amendments below. From 2026-11-03: every identity is namespaced by `provider`, and ingest writes at most one row per `(repository_id, event_type, source_ref)`. From 2026-11-05: a commit with more than one parent writes no row (clause 1a), so every commit carries a `parents` count. Commits arrive the way GitHub delivers them, inside a `push` delivery with a `commits` array, so one delivery can carry many commits.

- **Cycle:** 2026-09-01T00:00:00Z to 2026-09-30T23:59:59Z
- **Repository:** `acme-corp/finance-marts`, provider `github`, `repository_id = 41`, monitored for the whole cycle
- **Expected billable contributor count: 4**
- **Billable `(provider, provider_user_id)` pairs:** `github` 8801, 8802, 8804, 8806

## Two stages, because the ADR applies its rules in two places

ADR 0003 applies some rules when an event **arrives** (clauses 1–3: author not committer, opener counts, bots excluded by `user.type`). It applies the rest when billing **counts** (clause 5: distinct `(provider, provider_user_id)` pairs with a non-null id, over stored `contribution_event` rows). The `contribution_event` table has no column for account type, committer or trailers. So an ingest rule can only be tested by checking which rows ingest writes, not by looking at rows that are already stored.

- **Stage 1, ingest.** `ingest_input` is a list of normalized deliveries in delivery order. Commits arrive inside `push` deliveries, each carrying a `commits` array. This is not GitHub's literal webhook payload; it is the subset of fields ADR 0003 refers to. `parents` in particular is not in GitHub's push payload; ingest reads it from the compare API. Running ingest over it must produce exactly the rows in `expected_rows`.
- **Stage 2, billing.** Counting over `expected_rows` must produce `expected`.

A bot, a committer acting only as committer, a co-author named in a trailer, a reviewer, and a merge commit all produce **no row**. They are excluded at ingest, which is the only place the ADR can exclude them. A commit or pull request already stored for the repository also produces no row, whatever delivery it arrives in. A `provider_user_id: null` row means only *unresolved*. Bots must never be written that way, or they would be counted as unresolved authors.

## Scenario table

| # | Scenario | Clause | Rows written | Billable |
| --- | --- | --- | --- | --- |
| 1 | **Squash merge.** `dana` (8801) pushed 3 commits to a branch in one push, `d-0001`. `priya` (8807) squash-merged the PR, so git records priya as committer and dana as author of the squashed commit. Priya does nothing else this cycle. | 1: author, not committer | 4 rows, all 8801: 3 from `d-0001`, 1 from `d-0004` | 8801 |
| 2 | **Co-authored commit.** `mina` (8802) committed with `Co-authored-by: Sam Okafor <sam@acme.com>`. Sam is an invited member whose known email is `sam@acme.com` and whose provider id is 8803, so a trailer *could* be resolved by email. | 1: trailers count only the author | 1 row, 8802 | 8802 |
| 3 | **Bot.** `dependabot[bot]` (49699333, `type: "Bot"`) pushed a commit and opened a PR. | 3: excluded by account type | none | — |
| 4 | **PR opened by a non-committer.** `raj` (8804) opened PR #212, which carries only dana's commit. | 2: the opener counts | 2 rows: 8804 opener, 8801 author | 8804 (8801 already billable) |
| 5 | **Reviewer only.** `lena` (8805) approved PR #212. | 2: review is free | none | — |
| 6 | **Unresolved author.** A commit authored by `noreply@localhost`, with no resolvable provider identity. | 4: stored, never counted | 1 row, null | — |
| 7 | **Control.** `tomas` (8806) pushed an ordinary commit to `main`. | 1 | 1 row, 8806 | 8806 |
| 8 | **Same commit in two deliveries.** tomas's commit from scenario 7 (same SHA) arrives a second time in `d-0013`, a new delivery, as part of a later push to `main`. | Amendment 2026-11-03: one row per commit | none (`d-0012`'s row stands) | — (8806 already billable) |
| 9 | **Merge-button merge commit.** `nadia` (8808), who is not otherwise active this cycle, merges a pull request with the merge button. The push to `main`, `d-0014`, carries the merge commit, which has two parents. | Amendment 2026-11-05, clause 1a: merge commits are not billable | none | — |

Deliveries and commits do not match one to one, in three ways. `d-0001` carries three commits, so one delivery writes three rows. `d-0004` is redelivered with the same `delivery_id` and the same SHA. tomas's commit arrives again in `d-0013`, with a new `delivery_id` and the same SHA as `d-0012`. Ingest must key on the contribution itself, `(repository_id, event_type, source_ref)`, not on `delivery_id`.

**Totals:** 13 deliveries in (12 unique `delivery_id` values, plus the redelivery of `d-0004`). The ten `push` deliveries carry 12 commits. 9 rows written. The rows have four distinct non-null `(provider, provider_user_id)` pairs, plus one null row.

## The five traps, and what each one catches

Each trap is set so that a specific wrong implementation produces a specific wrong answer.

1. **Billing the committer.** Priya (8807) appears *only* as the committer of the squash commit. An implementation that writes a row for the committer bills her, so the count becomes 5 and 8807 appears.
2. **Resolving co-author trailers.** Sam's email matches a known identity, so an implementation that resolves `Co-authored-by` by email can find him. If it does, 8803 appears and the count becomes 5. Under the ADR the trailer is free text and is never resolved.
3. **Storing reviews, or excluding bots by name.** Lena (8805) billed means reviews were stored. Writing the bot's events produces either 49699333 in the count (count 5) or an extra unresolved row (`unresolved_event_count == 3` rather than 1). The bot's `login` deliberately ends in `[bot]` *and* its `type` is `"Bot"`, so a name rule and the type rule agree here. A separate assertion requires exclusion to key on `type`.
4. **Treating a delivery as a contribution.** Deliveries and commits do not match one to one, and the fixture breaks that assumption in all three ways:
   - **A delivery redelivered.** `d-0004` arrives twice, with the same `delivery_id` and the same SHA. Someone redelivered it by hand or through the REST API; GitHub does not redeliver on its own.
   - **One commit in several deliveries.** tomas's commit arrives in `d-0012` and again in `d-0013`.
   - **Several commits in one delivery.** `d-0001` carries dana's three branch commits.

   Ingest keyed on the contribution, `(repository_id, event_type, source_ref)`, writes 9 rows. Ingest that only skips a repeated `delivery_id` writes 10. Ingest that writes at most one row per delivery writes 8, and keeping that rule alongside the contribution key writes 7. Ingest with no idempotency writes 11. The billable count stays 4 in every case because it counts distinct pairs, which is exactly why the trap asserts the row count rather than the billable count.
5. **Billing the merge commit.** nadia (8808) appears only as the author and committer of `d-0014`'s two-parent commit. An implementation that writes a row for every commit bills her: `rows_written` becomes 10, the count becomes 5, and 8808 appears.

## JSON fixture

```json
{
  "fixture": "adr-0003-worked-example",
  "version": 5,
  "cycle": { "start": "2026-09-01T00:00:00Z", "end": "2026-09-30T23:59:59Z" },
  "repository": { "repository_id": 41, "provider": "github", "full_name": "acme-corp/finance-marts" },

  "known_identities": [
    { "provider": "github", "provider_user_id": 8803, "login": "sokafor", "email": "sam@acme.com",
      "member_source": "invited",
      "note": "pre-existing invited member; present so a trailer COULD be resolved by email" }
  ],

  "ingest_input": [
    { "delivery_id": "d-0001", "scenario": 1, "event": "push",
      "note": "one push to dana's branch carrying three commits; writes three rows, all with delivery_id d-0001",
      "commits": [
        { "sha": "1f6bc2fe1c50fdccb04974967035b6b02c00c244", "parents": 1, "occurred_at": "2026-09-03T09:14:22Z",
          "author":    { "provider": "github", "id": 8801, "login": "dana",  "type": "User" },
          "committer": { "provider": "github", "id": 8801, "login": "dana",  "type": "User" }, "trailers": [] },
        { "sha": "dbb944b05b566469f3595662a47566a75426e4c0", "parents": 1, "occurred_at": "2026-09-03T09:41:07Z",
          "author":    { "provider": "github", "id": 8801, "login": "dana",  "type": "User" },
          "committer": { "provider": "github", "id": 8801, "login": "dana",  "type": "User" }, "trailers": [] },
        { "sha": "497ebae0bb7c24246183227f26bf77e6869e8727", "parents": 1, "occurred_at": "2026-09-03T10:05:51Z",
          "author":    { "provider": "github", "id": 8801, "login": "dana",  "type": "User" },
          "committer": { "provider": "github", "id": 8801, "login": "dana",  "type": "User" }, "trailers": [] }
      ] },
    { "delivery_id": "d-0004", "scenario": 1, "event": "push",
      "note": "push of the squash commit to main: author dana, committer priya",
      "commits": [
        { "sha": "8e228f3f501bf47f6ebf61a7235a7b6b763b894e", "parents": 1, "occurred_at": "2026-09-03T15:02:18Z",
          "author":    { "provider": "github", "id": 8801, "login": "dana",  "type": "User" },
          "committer": { "provider": "github", "id": 8807, "login": "priya", "type": "User" }, "trailers": [] }
      ] },

    { "delivery_id": "d-0005", "scenario": 2, "event": "push",
      "commits": [
        { "sha": "786ad9038828af19ef2f12c88dfcbb3f1ec66a7e", "parents": 1, "occurred_at": "2026-09-08T14:02:55Z",
          "author":    { "provider": "github", "id": 8802, "login": "mina",  "type": "User" },
          "committer": { "provider": "github", "id": 8802, "login": "mina",  "type": "User" },
          "trailers":  ["Co-authored-by: Sam Okafor <sam@acme.com>"] }
      ] },

    { "delivery_id": "d-0006", "scenario": 3, "event": "push",
      "commits": [
        { "sha": "cd3033592332f5789c65e84bea24a2c414067813", "parents": 1, "occurred_at": "2026-09-11T02:00:31Z",
          "author":    { "provider": "github", "id": 49699333, "login": "dependabot[bot]", "type": "Bot" },
          "committer": { "provider": "github", "id": 49699333, "login": "dependabot[bot]", "type": "Bot" }, "trailers": [] }
      ] },
    { "delivery_id": "d-0007", "scenario": 3, "event": "pull_request_opened", "occurred_at": "2026-09-11T02:00:44Z",
      "pr_number": 209,
      "sender":    { "provider": "github", "id": 49699333, "login": "dependabot[bot]", "type": "Bot" } },

    { "delivery_id": "d-0008", "scenario": 4, "event": "push",
      "commits": [
        { "sha": "0e8b588a68b2be41320a7150b7b110906824a5cd", "parents": 1, "occurred_at": "2026-09-17T10:58:13Z",
          "author":    { "provider": "github", "id": 8801, "login": "dana",  "type": "User" },
          "committer": { "provider": "github", "id": 8801, "login": "dana",  "type": "User" }, "trailers": [] }
      ] },
    { "delivery_id": "d-0009", "scenario": 4, "event": "pull_request_opened", "occurred_at": "2026-09-17T11:20:09Z",
      "pr_number": 212,
      "note": "PR #212, carrying only the d-0008 commit",
      "sender":    { "provider": "github", "id": 8804, "login": "raj",   "type": "User" } },

    { "delivery_id": "d-0010", "scenario": 5, "event": "pull_request_review", "occurred_at": "2026-09-17T13:44:00Z",
      "state": "approved",
      "sender":    { "provider": "github", "id": 8805, "login": "lena",  "type": "User" } },

    { "delivery_id": "d-0011", "scenario": 6, "event": "push",
      "commits": [
        { "sha": "9f05106e0535e2f6967901417e65e41aa4d86d28", "parents": 1, "occurred_at": "2026-09-22T16:31:40Z",
          "author":    { "provider": "github", "id": null, "login": null, "type": null, "email": "noreply@localhost" },
          "committer": { "provider": "github", "id": null, "login": null, "type": null, "email": "noreply@localhost" }, "trailers": [] }
      ] },

    { "delivery_id": "d-0012", "scenario": 7, "event": "push",
      "commits": [
        { "sha": "426aef09650301f8ec905fbc4d889fbfaa358adb", "parents": 1, "occurred_at": "2026-09-28T08:12:04Z",
          "author":    { "provider": "github", "id": 8806, "login": "tomas", "type": "User" },
          "committer": { "provider": "github", "id": 8806, "login": "tomas", "type": "User" }, "trailers": [] }
      ] },

    { "delivery_id": "d-0004", "scenario": 1, "event": "push",
      "note": "manual redelivery of d-0004 (GitHub does not redeliver automatically); must not write a second row",
      "commits": [
        { "sha": "8e228f3f501bf47f6ebf61a7235a7b6b763b894e", "parents": 1, "occurred_at": "2026-09-03T15:02:18Z",
          "author":    { "provider": "github", "id": 8801, "login": "dana",  "type": "User" },
          "committer": { "provider": "github", "id": 8807, "login": "priya", "type": "User" }, "trailers": [] }
      ] },

    { "delivery_id": "d-0013", "scenario": 8, "event": "push",
      "note": "a later push to main carrying tomas's d-0012 commit again; same SHA, new delivery_id; must not write a second row",
      "commits": [
        { "sha": "426aef09650301f8ec905fbc4d889fbfaa358adb", "parents": 1, "occurred_at": "2026-09-28T08:12:04Z",
          "author":    { "provider": "github", "id": 8806, "login": "tomas", "type": "User" },
          "committer": { "provider": "github", "id": 8806, "login": "tomas", "type": "User" }, "trailers": [] }
      ] },

    { "delivery_id": "d-0014", "scenario": 9, "event": "push",
      "note": "push to main carrying a merge commit made with the merge button (two parents); writes no row",
      "commits": [
        { "sha": "68340b6fe5370dc3512f732794f440c1602afcf7", "parents": 2, "occurred_at": "2026-09-29T09:30:12Z",
          "author":    { "provider": "github", "id": 8808, "login": "nadia", "type": "User" },
          "committer": { "provider": "github", "id": 8808, "login": "nadia", "type": "User" }, "trailers": [] }
      ] }
  ],

  "expected_rows": [
    { "delivery_id": "d-0001", "repository_id": 41, "provider": "github", "provider_user_id": 8801, "event_type": "commit",              "source_ref": "1f6bc2fe1c50fdccb04974967035b6b02c00c244", "occurred_at": "2026-09-03T09:14:22Z" },
    { "delivery_id": "d-0001", "repository_id": 41, "provider": "github", "provider_user_id": 8801, "event_type": "commit",              "source_ref": "dbb944b05b566469f3595662a47566a75426e4c0", "occurred_at": "2026-09-03T09:41:07Z" },
    { "delivery_id": "d-0001", "repository_id": 41, "provider": "github", "provider_user_id": 8801, "event_type": "commit",              "source_ref": "497ebae0bb7c24246183227f26bf77e6869e8727", "occurred_at": "2026-09-03T10:05:51Z" },
    { "delivery_id": "d-0004", "repository_id": 41, "provider": "github", "provider_user_id": 8801, "event_type": "commit",              "source_ref": "8e228f3f501bf47f6ebf61a7235a7b6b763b894e", "occurred_at": "2026-09-03T15:02:18Z" },
    { "delivery_id": "d-0005", "repository_id": 41, "provider": "github", "provider_user_id": 8802, "event_type": "commit",              "source_ref": "786ad9038828af19ef2f12c88dfcbb3f1ec66a7e", "occurred_at": "2026-09-08T14:02:55Z" },
    { "delivery_id": "d-0008", "repository_id": 41, "provider": "github", "provider_user_id": 8801, "event_type": "commit",              "source_ref": "0e8b588a68b2be41320a7150b7b110906824a5cd", "occurred_at": "2026-09-17T10:58:13Z" },
    { "delivery_id": "d-0009", "repository_id": 41, "provider": "github", "provider_user_id": 8804, "event_type": "pull_request_opened", "source_ref": "212",                                      "occurred_at": "2026-09-17T11:20:09Z" },
    { "delivery_id": "d-0011", "repository_id": 41, "provider": "github", "provider_user_id": null, "event_type": "commit",              "source_ref": "9f05106e0535e2f6967901417e65e41aa4d86d28", "occurred_at": "2026-09-22T16:31:40Z" },
    { "delivery_id": "d-0012", "repository_id": 41, "provider": "github", "provider_user_id": 8806, "event_type": "commit",              "source_ref": "426aef09650301f8ec905fbc4d889fbfaa358adb", "occurred_at": "2026-09-28T08:12:04Z" }
  ],

  "expected": {
    "rows_written": 9,
    "billable_count": 4,
    "billable_identities": [
      { "provider": "github", "provider_user_id": 8801 },
      { "provider": "github", "provider_user_id": 8802 },
      { "provider": "github", "provider_user_id": 8804 },
      { "provider": "github", "provider_user_id": 8806 }
    ],
    "unresolved_event_count": 1,
    "not_billed": [
      { "provider": "github", "provider_user_id": 8807,     "reason": "committer_only",                   "excluded_at": "ingest" },
      { "provider": "github", "provider_user_id": 8803,     "reason": "co_author_trailer_not_resolved",   "excluded_at": "ingest" },
      { "provider": "github", "provider_user_id": 49699333, "reason": "account_type_bot",                 "excluded_at": "ingest" },
      { "provider": "github", "provider_user_id": 8805,     "reason": "review_only",                      "excluded_at": "ingest" },
      { "provider": "github", "provider_user_id": 8808,     "reason": "merge_commit",                     "excluded_at": "ingest" }
    ]
  },

  "assertions": [
    "stage 1: ingest over ingest_input writes exactly expected_rows (set equality on delivery_id + provider + provider_user_id + event_type + source_ref)",
    "stage 1: rows_written == 9 — the redelivered d-0004, the repeated commit in d-0013 and the merge commit in d-0014 write nothing",
    "stage 1: a commit SHA already stored for the repository writes no row, whatever its delivery_id",
    "stage 1: a delivery carrying N commits writes N rows, all with the same delivery_id",
    "stage 1: a commit with more than one parent writes no row, whoever authored it",
    "stage 1: no row has (provider, provider_user_id) in {(github, 8803), (github, 8805), (github, 8807), (github, 8808), (github, 49699333)}",
    "stage 1: bot exclusion keys on author/sender type == 'Bot', never on login; a test that sets login to 'dependabot' (no [bot] suffix) with type 'Bot' must still write no row",
    "stage 1: no Co-authored-by trailer is resolved, even when its email matches known_identities",
    "stage 2: billable_count == count(DISTINCT (provider, provider_user_id)) over expected_rows WHERE provider_user_id IS NOT NULL == 4",
    "stage 2: set(billable) == {(github, 8801), (github, 8802), (github, 8804), (github, 8806)}",
    "stage 2: (github, 8801) has 5 rows and counts once",
    "stage 2: unresolved_event_count == 1",
    "ADR 0004: each of (github, 8801), (github, 8802), (github, 8804), (github, 8806) has an organization_member row with source = 'contribution' and NO workspace_role",
    "ADR 0004: (github, 8803) keeps its existing member row with source = 'invited'; ingest creates no second member row and grants no role"
  ]
}
```

## Edge cases this fixture does not cover

Each needs its own fixture before Phase 5 ships, and each needs a decision the ADR does not make:

- **A commit authored inside the cycle but delivered after it closes.** Its `occurred_at` places it in the earlier cycle, so an invoice that was already issued becomes wrong after the fact. Whether that reopens the invoice or carries the charge forward is a billing-policy decision, not an ADR 0003 one.
- **A repository that stops being monitored mid-cycle.** Its events stay stored. Whether they remain billable is not tested here.
- **A contributor active in two monitored repositories in one workspace.** They should count once for the workspace. Assert it explicitly, because grouping by repository is the easy mistake.

## Amendment 2026-11-03

Phase 1 review found two schema gaps. This amendment adds to the decision above and reverses
nothing in it; the status stays Accepted. Where the schema below differs from the schema
consequence above, this one applies. The worked example above covers
both changes.

### 1. Identities are namespaced by provider

A numeric user id is unique only within one provider: GitHub user `8801` and GitLab user `8801`
are different people. Wherever `provider_user_id` identifies a person, it is paired with
`provider`:

- `contribution_event` gains a `provider` column (schema below).
- Clause 4 deduplicates on the pair, not on `provider_user_id` alone.
- Billable contributors are distinct `(provider, provider_user_id)` pairs. The billing count is the
  number of distinct pairs with a non-null `provider_user_id` among the period's events.

This ADR chose a stable numeric id over a username or email so that renames do not turn one person
into two, and namespacing it by provider keeps that guarantee across providers.

`provider` is never null, even on an unresolved event: the provider an event came from is always
known, even when its author is not. It is the provider of the event's repository. Unresolved still
means `provider_user_id` is null, and an unresolved event is still never counted.

### 2. A contribution is identified by what it is, not by the delivery that carried it

`delivery_id UNIQUE` assumed that one delivery writes one row. That fails in both directions:

- One push delivery carries many commits, so one delivery can write many rows. With `delivery_id`
  unique, every commit after the first in a push would be rejected.
- The same commit can arrive in more than one delivery: pushed to a branch, then delivered again
  when the branch is merged. Its new `delivery_id` would let it write a second row.

A contribution is identified by its repository, its event type and what it refers to:

```
contribution_event
  id
  repository_id        NOT NULL
  provider             NOT NULL   -- the repository's provider; never null
  provider_user_id     NULL       -- null = unresolved, never counted
  event_type                      -- 'commit' | 'pull_request_opened'
  source_ref           NOT NULL   -- commit SHA for 'commit'; PR number for 'pull_request_opened'
  occurred_at          NOT NULL   -- authored/opened time, not received time
  delivery_id                     -- the delivery that wrote this row; not unique
  UNIQUE (repository_id, event_type, source_ref)
```

Ingest writes a row only when no row with the same `(repository_id, event_type, source_ref)`
exists. A repeat writes nothing, whatever its `delivery_id`, and the stored row keeps the
`delivery_id` of the delivery that first wrote it. This key is now what makes redelivery
idempotent: a redelivered webhook carries the same commits and pull request, so it collides on the
same key. It also enforces clause 5's "one row per commit and pull request", and it lets the
Phase 2 reconciliation job backfill from the provider's API without duplicating anything already
stored.

**Push payloads are capped.** GitHub's push webhook does not always carry every pushed commit. Its
documentation says of the payload's `commits` array:

> The array includes a maximum of 2048 commits. If necessary, you can use the Commits API to fetch
> additional commits.

Source: GitHub Docs, [Webhook events and payloads, `push`](https://docs.github.com/en/webhooks/webhook-events-and-payloads#push),
checked 2026-11-03.

The payload carries no total commit count, so an array of exactly 2048 commits is the only sign
that commits were left out. When a push delivery's `commits` array holds 2048 commits, ingest
fetches the push's `before...after` range through the compare API,
[`GET /repos/{owner}/{repo}/compare/{basehead}`](https://docs.github.com/en/rest/commits/commits#compare-two-commits),
and writes the commits the payload left out. That endpoint has its own limit: called without
paging parameters it returns at most 250 commits, so ingest must paginate it (`per_page` is at most
100). Commits fetched this way take the push delivery's `delivery_id`, and the unique key makes it
harmless to fetch commits the payload already carried.

## Amendment 2026-11-05

This amendment adds to the decision and to the 2026-11-03 amendment above, and reverses nothing in
them; the status stays Accepted. The worked example above covers clause 1a.

### 1. Redelivery is manual; recovering missed deliveries is the reconciliation job's

The schema consequence above says "providers redeliver". GitHub does not, on its own:

> GitHub does not automatically redeliver failed deliveries.

Source: GitHub Docs, [Handling failed webhook deliveries](https://docs.github.com/en/webhooks/using-webhooks/handling-failed-webhook-deliveries),
checked 2026-11-05.

A failed delivery is redelivered only when someone redelivers it, from the webhook's settings or
through the REST API, and only within the past 3 days on GitHub.com (7 on GitHub Enterprise Server):
[Redelivering webhooks](https://docs.github.com/en/webhooks/testing-and-troubleshooting-webhooks/redelivering-webhooks).
Automatic redelivery exists only as a script that the webhook's owner writes and schedules.

Idempotency on the contribution key, `(repository_id, event_type, source_ref)`, still matters:
manual redeliveries happen, and overlapping pushes carry the same commit in more than one delivery.
But a delivery that failed and was never redelivered is simply missing, so **recovering missed
deliveries is the reconciliation job's responsibility**. The job walks the provider's API for the
period, as the Consequences above already require, and writes each contribution it finds that is
not yet stored, with `delivery_id` null, since no delivery carried it. The contribution key stops
it from duplicating a row that a delivery already wrote.

### 2. Clause 1a: merge commits are not billable

**1a. A commit with more than one parent writes no contribution row.** A merge commit adds no
authored change of its own, and the commits it brings in are already counted for their authors.
Billing the person who pressed the merge button would contradict clause 1's treatment of a
committer who only merges.

Scope:

- **Every commit with two or more parents**, whoever authored it and however it was made: the
  merge button, `git merge`, or `git merge --no-ff`.
- **Not squash merges or rebase merges.** Those produce single-parent commits, and clause 1
  already covers them: each counts for its author.
- **Merge commits that resolve conflicts are excluded too.** This is a deliberate simplification.
  Conflict resolution is real work, but it can't be told apart from a clean merge without diffing,
  and this ADR does not bill on diffs.

**Applying it.** The test is the parent count, never the commit message, which is free text.
GitHub's push payload does not carry the count: its commit objects have `id`, `tree_id`,
`distinct`, `message`, `timestamp`, `url`, `author`, `committer`, `added`, `removed` and
`modified`, and no parents (the `webhook-push` schema in GitHub's
[OpenAPI description](https://github.com/github/rest-api-description), checked 2026-11-05).
Ingest reads each commit's parents from the
[compare API](https://docs.github.com/en/rest/commits/commits#compare-two-commits), whose commit
objects include `parents`, or from the single-commit endpoint. Every push that carries commits
therefore needs an API call, not only a truncated one.
