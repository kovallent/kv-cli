# 0004 — Membership and roles

- **Status:** Proposed
- **Date:** 2026-09-04
- **Deciders:** platform
- **Supersedes:** none
- **Depends on:** ADR 0002 (tenancy), ADR 0003 (active contributor)
- **Blocks:** Phase 1 (ingest spine database schema), Phase 4 (auth and RBAC), Phase 6 (SSO)

## Context

ADR 0002 defines which workspace owns a repository. ADR 0003 defines who is billed. Neither
defines who can *sign in*, what they can see, or what they can change — and the two existing ADRs
make that gap load-bearing rather than cosmetic.

Two populations meet here:

- **Active contributors** (ADR 0003) are derived from webhook events — someone who commits to or
  opens a pull request against a monitored repository.
- **Members** are people the organization knows about: an identity record, and optionally access.

**Every billable contributor is a member.** A customer must be able to see, in one list, every
person they are charged for; two lists that have to be reconciled by hand is an invoice dispute
waiting to happen. Membership itself remains free and uncapped — what is capped is repositories
(by tier) and what is billed is contributors (by activity).

That coupling forces a distinction. Membership currently means two things at once — *we know who
you are* and *you may sign in and read findings*. If contributing makes you a member, and
membership grants access, then anyone who commits to a monitored repository can read every
finding in the workspace. That is a security change nobody asked for, so the two meanings have to
be separated.

The authority question is the sharper one. Deploying a contract or editing guardrail config is the
action that can weaken enforcement across every repository in a workspace — the same authority
problem ADR 0001 answers for the CI pin, where the expected hash deliberately lives somewhere the
pull-request author cannot edit. If any member can disable KV003 for 34 repositories, the pin
decision is undermined from inside the product.

## Decision

**1. A user is a member of an organization, with roles assigned per workspace.**

Identity and membership are organization-level; authority is workspace-level. One invitation, one
SSO assertion, one deactivation — then zero or more workspace role grants beneath it.

This is chosen over independent per-workspace membership because it is the only shape that
supports enterprise SSO and SCIM cleanly: the identity provider is authoritative for *who is in
the organization*, and Kovallent remains authoritative for *what they may do in each workspace*.
Independent membership is simpler but forces deprovisioning to fan out across every workspace,
and a missed workspace on offboarding is a security finding.

An organization member with no workspace role grant can sign in and see nothing but the workspace
list. That is deliberate: it separates "has an account" from "has access."

**Membership is an identity record. Access is a workspace role.** These are separate rows, and
one does not imply the other.

**A member is provisioned either by invitation or by contribution.** Invitation is the familiar
path — an admin or the identity provider adds someone, and a role grant usually accompanies it.
Contribution is automatic: the first billable event from an unrecognised provider identity (ADR
0003) creates a member record with `source = 'contribution'` and **no workspace role**. That
person is visible in the members list, attributable on the invoice, and cannot sign in and read
anything until someone grants them a role.

Promotion is an ordinary role grant. Nothing about the member record changes.

**2. Three workspace roles.**

| Role | May |
| --- | --- |
| **Owner** | Everything Admin may, plus billing, tier changes, transferring repositories between workspaces (ADR 0002), and workspace deletion |
| **Admin** | Deploy and archive contracts, edit guardrail config, add and remove monitored repositories, manage role grants below Owner |
| **Member** | Read dashboards and findings, run audits, view contracts. No authorship |

The Owner/Admin split exists for exactly one reason: billing and repository transfer are the two
actions that change what a customer is charged, and they should not sit with everyone who is
allowed to author a contract.

The Admin/Member split exists because contract deployment and guardrail config are enforcement
authority, not preferences. Members can run an audit — they cannot change what an audit means.

**3. A read grant confers visibility on every member of the granted workspace, automatically.**

When workspace A holds a read grant over a repository owned by workspace B (ADR 0002), every
member of A sees that repository at their A role. There is no second, per-user layer of repository
permissions.

One permission system, evaluated in one place:

```
can_read(user, repository) :=
     role_in(user, repository.workspace_id) is not null          -- owner workspace
  or role_in(user, W) is not null for some W with a grant on repository
```

The alternative — per-user repository ACLs on top of workspace grants — was rejected because two
permission systems that can disagree eventually do, and the resulting question ("why can Dana see
this repo?") becomes unanswerable without a trace tool. A grant is a deliberate administrative act
between workspaces; making it also require per-user work means it will be granted too broadly to
avoid the second step.

Note the asymmetry, which is intended: a grant conveys **read only**. Authority over a repository
— contracts, guardrail config, removal — always follows singular ownership. An Admin of A cannot
change enforcement for a repository owned by B.

### Schema consequence

```
organization_member
  organization_id
  user_id
  status                          -- 'active' | 'invited' | 'deactivated'
  source                          -- 'invited' | 'contribution'
  provider_user_id     NULL       -- numeric provider id; links to contribution_event
  provider_subject     NULL       -- SSO subject, when federated
  PRIMARY KEY (organization_id, user_id)

workspace_role
  workspace_id
  user_id
  role                            -- 'owner' | 'admin' | 'member'
  PRIMARY KEY (workspace_id, user_id)   -- exactly one role per workspace
```

`workspace_role` is keyed so a user holds exactly one role per workspace; there is no role union
to resolve at request time. Every workspace must have at least one Owner — enforced on grant
removal and on the last-Owner case, not left to the UI.

`provider_user_id` is what joins a member to their `contribution_event` rows (ADR 0003), and it is
the provider's numeric id for the same reason stated there — usernames and emails move, the id does
not. A contribution event whose identity cannot be resolved still stores `provider_user_id: null`
and creates no member; it is counted as unattributed rather than billed, so an unresolvable commit
never produces a charge without a name against it.

Deactivating an organization member is the single revocation point: it does not delete
`workspace_role` rows, so reinstatement restores prior access, and it does not touch
`contribution_event` rows, so historical billing stays recomputable per ADR 0003.

## Consequences

**Offboarding is one operation.** Deactivate the organization member and every workspace role is
inert. This is the property that makes SCIM deprovisioning correct rather than best-effort.

**Membership is free and unbounded, which is the intended adoption path.** Inviting a whole team
to read dashboards costs nothing, and none of them become billable unless they commit or open a
pull request in a monitored repository.

**Every charge is attributable to a row in the members list.** "Why is this invoice 12
contributors?" is answered by filtering that list — no second list, no reconciliation.

**Membership does not imply billing.** The converse of this ADR's premise is emphatically not
true: an invited member who never commits is free forever. The members list therefore shows a
mixture of billable and non-billable people, and the billable flag must be visible per row and
labelled as derived from activity in the current cycle. A member who stops contributing stops
being billable while remaining a member.

**Deactivation cannot retract a charge.** Because billing derives from stored events (ADR 0003),
deactivating a member mid-cycle removes their access but not the commits they already made. The
settings UI must not imply otherwise — "remove member" is an access action, not a billing action.

**Contribution-sourced members arrive without being invited, which is a surprise unless surfaced.**
The members list will grow on its own. It should distinguish invited from contribution-sourced at
a glance, and granting a role to a contribution-sourced member should read as promotion rather
than as first contact.

**Read grants are coarse by design.** Granting workspace A visibility over a repository exposes it
to all of A's members. If a customer needs finer separation, the answer is a narrower workspace,
not a per-user exception — this should be said in the product, at the point of granting.

**Roles do not gate the CLI.** `kv-cli` is local, unauthenticated, and free forever. Roles govern
the control plane and the CI gate only. A Member who cannot deploy a contract can still audit
locally against the deployed one, which is the correct outcome.

## Rejected alternatives

**Independent per-workspace membership.** Simpler schema, no organization layer. Rejected because
deprovisioning fans out and a missed workspace is a security finding — and because SSO has to bind
to something org-level anyway.

**Two roles (Admin, Member).** Fewer concepts, and it puts billing and repository transfer in the
hands of everyone who may author a contract. Those are the actions that change the invoice, so
they get their own role.

**Four or more roles (adding Billing, or Auditor).** A read-only Auditor is already expressible as
Member; a Billing-only role is a real enterprise ask but is better added when a customer names it
than guessed at now. The role set is additive later; taking a role away is not.

**Per-user repository ACLs layered on grants.** Rejected above: two permission systems that can
disagree, and a strong incentive to over-grant to avoid the second step.

**Billable contributors independent of membership** (the first draft of this ADR). Contributors
were derived purely from webhook events and needed no member record, so the settings page showed
two counts that had to be reconciled by eye. Rejected because a charge with no corresponding row
in the members list is not defensible in an invoice dispute.

**Auto-granting the Member role on first contribution.** The obvious shortcut, and it hands read
access to every finding in the workspace to anyone who lands a commit — including outside
contributors to a public repository. Identity is created automatically; access never is.
