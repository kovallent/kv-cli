# 0002 — Tenancy: a repository belongs to exactly one workspace

- **Status:** Accepted
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

## Amendment 2026-11-03

Phase 1 review found a schema gap. This amendment adds to the decision above and reverses nothing
in it; the status stays Accepted.

**A repository is keyed by its provider and its provider id.** A numeric provider id is unique only
within one provider: GitHub repository `8801` and GitLab repository `8801` are different
repositories. `repository` gains a `provider` column, and `provider_repo_id` is no longer unique on
its own; the pair is.

```
repository
  id
  workspace_id        NOT NULL   -- ownership; exactly one
  provider            NOT NULL   -- the code host, e.g. 'github', 'gitlab'
  provider_repo_id               -- numeric provider id, unique only within its provider
  ...
  UNIQUE (provider, provider_repo_id)
```

This ADR chose the numeric `provider_repo_id` over `owner/name` so that a rename cannot turn one
repository into two, and namespacing it by provider keeps that guarantee across providers.

Nothing else changes. Ownership is still singular, the quota check still counts `repository` rows,
and `workspace_repository_grant` and `run` still reference `repository.id`, which is unaffected.

## Amendment 2026-11-13: grants stay inside one organization

This amendment adds to the decision above and reverses nothing in it; the status stays Accepted.

**A `workspace_repository_grant` may only grant a repository to a workspace in the same
organization as the repository's owning workspace.**

The organization is the tenant boundary. A grant across organizations would show one tenant's
findings, which include file paths, symbols and redacted credential details, to another tenant's
members. [ADR 0004](0004-membership-and-roles.md) makes every member of the granted workspace a
reader, so a single bad grant would expose a whole workspace.

**The database enforces the rule, not application code.** No code creates grants yet, and the
first code that does should not have to remember the rule. Each grant row names one organization,
and two composite foreign keys require both the granted workspace and the repository to belong to
it, so no code path can write a grant across organizations:

```
workspace
  id
  organization_id     NOT NULL
  UNIQUE (id, organization_id)

repository
  ...
  organization_id     NOT NULL   -- the owning workspace's organization
  FOREIGN KEY (workspace_id, organization_id) REFERENCES workspace (id, organization_id)
  UNIQUE (id, organization_id)

workspace_repository_grant
  workspace_id
  repository_id
  organization_id     NOT NULL
  PRIMARY KEY (workspace_id, repository_id)
  FOREIGN KEY (workspace_id, organization_id)  REFERENCES workspace (id, organization_id)
  FOREIGN KEY (repository_id, organization_id) REFERENCES repository (id, organization_id)
```

The same keys hold through a transfer (decision above): a repository with grants cannot move to a
workspace in another organization until those grants are revoked. A transfer within one
organization is unaffected.

**Sharing between organizations**, if it is ever needed, is a new decision with its own ADR. It is
not a relaxation of this rule.
