# 0002 — Tenancy: a repository belongs to exactly one workspace

- **Status:** Proposed
- **Date:** 2026-09-04
- **Deciders:** platform
- **Supersedes:** none
- **Blocks:** Phase 1 (ingest spine database schema), Phase 5 (billing)

## Context

The control plane presents three levels: organization, workspace, repository. The CLI has no
concept of any of them — it reads a local `.kovallent.yaml` and reports on the files it was
pointed at. Phase 1 introduces stored runs, and a stored run has to be attributed to something.

The question that has to be answered before that schema exists is whether one repository may
belong to two workspaces. It reads as an administrative preference and is in fact load-bearing
for billing: the pricing model counts *monitored repositories* against a tier quota, so if a
repository can appear under two workspaces, every quota check and every invoice line needs a
deduplication rule — and the rule has to be identical in the metering code and on the invoice.

Two real motivations for multi-workspace visibility exist and should not be dismissed:

1. A platform team wants a workspace containing every repository in the org for a compliance
   view, while each product team wants a workspace containing only theirs.
2. A repository is jointly owned during a migration and both teams want it on their dashboard.

Neither of those requires shared *ownership*. Both are satisfied by read access.

## Decision

**A repository belongs to exactly one workspace.** Ownership is singular, and ownership is what
billing counts.

**Visibility is a separate, read-scoped concept.** A workspace may be granted read access to
repositories it does not own. Those repositories appear in its dashboards and reports, and are
never counted against its quota or its invoice.

**A run whose repository cannot be resolved is stored unattributed.** When a run arrives with
`repo: null` — a local run, a fork, a repository the installation cannot see — it is persisted
with a null repository reference. An unattributed run is never quota-counted and never satisfies
a required check. It exists so that ingest is lossless and so the volume of unattributable runs
is measurable rather than invisible.

**Moving a repository between workspaces is an explicit, audited transfer,** not a side effect of
granting access. Historical runs stay attached to the repository, not to the workspace that owned
it at the time; a transfer therefore moves the repository's whole history with it. The transfer
event is recorded so a mid-cycle move is explicable on an invoice.

### Schema consequence

```
repository
  id
  workspace_id        NOT NULL   -- ownership; exactly one
  provider_repo_id    UNIQUE     -- numeric provider id, not "owner/name"
  ...

workspace_repository_grant
  workspace_id
  repository_id
  PRIMARY KEY (workspace_id, repository_id)   -- visibility only, read scope

run
  id
  repository_id       NULL       -- null = unattributed, never billed
  ...
```

`provider_repo_id` is the provider's numeric identifier, not `owner/name`. Repositories get
renamed and transferred between GitHub organizations, and a name-keyed row silently becomes a
second repository when that happens — which would double-count it against a quota.

## Consequences

**Quotas become countable by construction.** `SELECT count(*) FROM repository WHERE workspace_id
= ? AND last_scanned_at > now() - interval '30 days'` is the whole quota check. No deduplication
pass, no reconciliation between what the meter counted and what the invoice charged.

**The platform-team compliance view costs a grant, not a duplicate.** An org-wide workspace with
read grants over every repository produces the same dashboard without owning anything, so its
quota is zero.

**Joint ownership is not expressible.** The migration case above must nominate one owner. This is
the real cost of the decision, and it is accepted: the alternative pushes ambiguity into billing,
where it is far more expensive than a conversation between two teams.

**Unattributed runs need a product surface.** If a customer's local runs are all arriving with
`repo: null`, that is a misconfiguration they cannot currently see. A count of unattributed runs
per organization should appear in settings before Phase 5.

## Rejected alternatives

**Many-to-many ownership with a `primary_workspace_id` for billing.** Rejected because it stores
the same fact twice. The moment ownership and billing-ownership can disagree, they eventually do,
and the resulting invoice dispute is unanswerable from the data.

**Repositories owned by the organization, with workspaces as pure views.** Cleaner, and it makes
quotas an org-level concept — which contradicts the published pricing model, where the quota is
per tier and the tier is bought by a team. Revisit if pricing moves to org-level seats.

**No workspace level at all.** Sufficient for a single-team customer and fails immediately for
the enterprise tier, whose stated value is per-team policy separation.
