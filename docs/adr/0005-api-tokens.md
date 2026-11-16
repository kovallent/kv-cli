# 0005 — API tokens

- **Status:** Accepted
- **Date:** 2026-11-14
- **Deciders:** platform
- **Supersedes:** none
- **Depends on:** ADR 0001 (contract distribution), ADR 0002 (tenancy), ADR 0004 (membership and
  roles)
- **Blocks:** the MCP server; removing `X-Kovallent-Organization-Id` from ingest

## Context

The only credential today is a browser session, from GitHub sign-in. Three kinds of client can't
use one:

- **Agents, scripts and an MCP server** have no browser.
- **Uploads from CI** use GitHub OIDC (see [CI with the GitHub Action](../ci.md)). That only works
  inside GitHub Actions: GitLab CI, Jenkins or a laptop have no way to upload.
- **`X-Kovallent-Organization-Id`** is an unauthenticated stand-in: whoever sends the header picks
  the organization a run is stored under. The Phase 1 review said it must go once a real
  credential exists.

The control plane needs a credential that works without a browser and outside GitHub Actions. It
must not become a second permission system next to [ADR 0004](0004-membership-and-roles.md)'s,
and it must not give agents an authority over the gate that people don't have.

## Decision

**1. A token belongs to one person in one organization.** It carries exactly that person's roles
and read grants in that organization, through the same authorization function as a browser session,
and nothing more. A person in two organizations makes two tokens. One token per organization keeps
every request unambiguous about which tenant it's in.

Tokens are created and revoked by the person they belong to, while signed in. A token can't
create, extend or revoke tokens, since it has no write scope (decision 3).

**2. Access is checked on every request, not when the token is made.** Removing someone's role
removes their token's access at once. Deactivating the member makes all their tokens stop working.

```
authorize(token, request) :=
     sha256(presented token) matches token.token_hash          -- constant time; decision 4
 and token.revoked_at is null
 and now() < token.expires_at
 and request needs a scope in token.scopes                     -- decision 3
 and organization_member(token.organization_id, token.user_id).status = 'active'
 and the browser-session check (ADR 0004) passes for token.user_id,
     limited to workspaces and repositories in token.organization_id
```

Nothing is copied onto the token at creation except the person, the organization and the scopes.
Roles, grants and member status are read on each request, so ADR 0004's single revocation point
(deactivating the member) covers tokens too.

**3. Scopes.** `read` covers every read endpoint the person can use. `upload` covers posting a run.
A token has one or both. There's no write or admin scope; one is added only when write endpoints
exist, in an amendment to this ADR.

A scope narrows what a token can do; it never widens it. A `read` token for a person with no
workspace role reads what that person reads when signed in: the workspace list, and nothing else.

**4. Storage.** The server keeps only a SHA-256 hash of each token, never the token.

- A token is `kv_` followed by 32 random bytes from a cryptographically secure generator,
  base62-encoded to a fixed 43 characters (46 in all).
- That's 256 bits of entropy, so a fast hash is correct. A password hash (bcrypt, scrypt, Argon2)
  slows down guessing a low-entropy secret, and there's nothing here to guess. It would add CPU
  cost to every request and no protection.
- The first 8 characters after `kv_` are stored for display, so people can tell their tokens
  apart. They're not secret; the other 35 characters still carry over 200 bits.
- Lookup finds candidate rows by those 8 characters, then compares the SHA-256 of the presented
  token with each candidate's hash in constant time. The 8 characters aren't unique, so there can be
  more than one candidate.

**5. Lifetime.** Every token has an expiry, at most 365 days after it's made, and 90 days by
default. It can be revoked at any time. `last_used_at` is recorded.

**6. What agents may do with one.** Through the API or an MCP server, an agent can read findings
and runs, and upload a run. It can't change an expected contract hash, mark a gate passed, or change
anyone's access.

The gate remains kv-cli's exit code in CI ([ADR 0001](0001-contract-distribution.md)). A run
uploaded with a token is a record, not a gate result: it never satisfies a required check and
never sets a check-run conclusion, whether or not it's attributed to a repository. ADR 0002 already
says this of unattributed runs. For token uploads it holds for attributed runs too, because the
uploader asserts the repository and the results (decision 7).

kv-cli never fetches credentials, for an agent or anyone else. It stays local and unauthenticated
(ADR 0004, "Roles do not gate the CLI"). Whatever runs it (a script, an agent, the MCP server)
holds the token and uploads the JSON report.

**7. Uploads.**

- **From GitHub Actions, OIDC stays the way to upload.** It needs no stored secret, and the
  repository's identity comes from signed claims, not from the request. Tokens are for everything
  else. The Action takes no token input.
- **With a token:** if the run names a repository owned by a workspace in the token's
  organization, and the person has a role in that workspace, the run is attributed to it.
  Otherwise it's stored unattributed under the token's organization. A run is never attributed to a
  repository the person can't read.
- **`X-Kovallent-Organization-Id` is removed.** An upload with neither OIDC nor a token is
  refused. The dev-only unauthenticated path stays behind `KOVALLENT_ALLOW_UNAUTHENTICATED_INGEST`,
  off by default. It stores runs fully orphaned, with no organization and no repository, so no
  tenant ever sees them.

**How a run names a repository.** A run's only repository field is `run.repo`, `owner/name` (see
[JSON payload](../json-payload.md)). ADR 0002 doesn't use a name as a repository's identity, and
the field doesn't say which provider the repository is on. So a token upload resolves `run.repo`
strictly. It's attributed only when the name matches the current name of exactly one repository in
the token's organization. No match (a local run, a fork, an old name after a rename) or more than
one match (the same `owner/name` on two providers) stores the run unattributed. The role check comes
after the match, so a name can never attribute a run outside the workspaces where the person has a
role.

The role needed is a role in the **owning** workspace. A person who reads a repository only
through a read grant (ADR 0002) can't attribute runs to it: attributing a run adds to the owning
workspace's history, and ADR 0004 gives a grant read access only.

### Schema consequence

```
api_token
  id
  organization_id      NOT NULL
  user_id              NOT NULL
  display_prefix       NOT NULL   -- first 8 characters after 'kv_'; not secret, not unique
  token_hash           NOT NULL   -- SHA-256 of the whole token, 'kv_' included
  scopes               NOT NULL   -- non-empty subset of {'read', 'upload'}
  created_at           NOT NULL
  expires_at           NOT NULL   -- at most created_at + 365 days; 90 days by default
  revoked_at           NULL
  last_used_at         NULL
  UNIQUE (token_hash)
  FOREIGN KEY (organization_id, user_id) REFERENCES organization_member (organization_id, user_id)
  CHECK (expires_at <= created_at + interval '365 days')

run
  id
  organization_id      NULL       -- null only on the dev-only unauthenticated path
  repository_id        NULL       -- null = unattributed (ADR 0002)
  api_token_id         NULL       -- the token that uploaded it; null for OIDC
  ...
```

The foreign key to `organization_member` is what makes a token one person in one organization: it
can't name a person outside that organization. The token doesn't copy roles or member status;
`authorize` reads them on each request.

`run.api_token_id` records which token uploaded each run. If a token leaks, it lists exactly the
runs that token posted, so they can be found and, if need be, set aside.

## Consequences

**Agents, scripts and the MCP server can use the control plane.** They read findings and runs,
and upload runs, with one person's access in one organization, and with no browser. Uploads work
from any CI system or machine, not only GitHub Actions.

**Every stored run has an authenticated organization.** Once `X-Kovallent-Organization-Id` is
removed, an organization comes from OIDC claims or from a token, never from a header the caller
chose. The only exception is the dev-only path, whose runs belong to no organization.

**A leaked token is a leaked read credential for one person in one organization**, until it's
revoked or it expires. That's the main thing given up, and it's why every token has an expiry, and
why there are no organization-wide tokens. A token with `upload` can also post runs as that person:
unattributed under the organization, or attributed to repositories in workspaces where the person
has a role. It can't change a contract hash, pass a gate or change anyone's access.

**A token upload is asserted, not proven.** With OIDC, GitHub signs the repository's identity.
With a token, the uploader names the repository and supplies the results. That's acceptable for a
record, and it's why a token-uploaded run never satisfies a required check (decision 6).

**Organizations can't manage individual tokens.** ADR 0004 has no organization-level role, so
there's no one but the person who can list or revoke a person's tokens. The organization's control
is deactivating the member, which stops all of that person's tokens at once. A token view for
organization administrators would need a new role, in a new decision.

**Reinstating a member brings back their tokens.** Because access is checked on every request, a
deactivated member's tokens work again on reinstatement if they haven't expired or been revoked.
That matches ADR 0004, where reinstatement restores prior access. If a member left under suspicion,
revoke their tokens as well as deactivating them.

**Authorization can't be cached across requests** unless the cache is invalidated on every role
change, grant change, revocation and deactivation. Otherwise decision 2 stops being true. Every
request pays a token lookup and a role check.

**Tokens expire, so long-running scripts break.** At most once a year, and every 90 days by
default. `last_used_at` shows which tokens are still in use; warning people before a token expires
needs a product surface.

**kv-cli doesn't recognize its own tokens yet.** KV002 matches the shapes of GitHub, Slack, AWS and
other keys by value. The fixed `kv_` prefix and length make a Kovallent token matchable the same
way, and adding that shape is a follow-up.

## Rejected alternatives

**Organization-wide tokens (service accounts).** Convenient for shared automation, and a leak
exposes every workspace in the tenant, with no person to deactivate. ADR 0004 makes the member the
single revocation point; a token with no member has none.

**One token across all of a person's organizations.** Fewer tokens to manage, and every request
would have to say which tenant it means, and a leak would span tenants. One token per organization
makes the tenant part of the credential.

**Permissions copied onto the token when it's made.** Cheaper to check, and removing a role or
deactivating a member would leave every token they made working. Offboarding would stop being one
operation, which ADR 0004 depends on.

**A password hash for token storage.** bcrypt, scrypt and Argon2 slow down guessing low-entropy
secrets. A 256-bit random token can't be guessed, so a slow hash only adds CPU cost to every
request.

**Tokens that never expire.** They outlive the scripts and laptops they were made for, and a leak
lasts until someone notices it.

**Keeping `X-Kovallent-Organization-Id` alongside tokens.** It would leave an unauthenticated way
to write runs into any organization, which is the problem this ADR exists to remove.

**Tokens instead of OIDC in GitHub Actions.** One credential for every CI system, and every
repository would need a stored secret that can leak, where OIDC needs none and proves the
repository.

**A write or admin scope now.** There are no write endpoints for it to cover. Scopes are additive
later, like ADR 0004's roles; taking one away from tokens already issued is not.

**kv-cli fetching or storing a token (`kv-cli login`).** It would make the CLI depend on an account
and a network, which ADR 0001 and ADR 0004 rule out, and it would put a credential where an agent
running kv-cli could read it.

## Amendment 2026-11-15

This amendment corrects two statements above and adds four rules. The text above is unchanged;
where the two disagree, this amendment applies.

### 1. An attributed run takes its organization from its repository

An attributed run doesn't store `organization_id`. It reaches its organization through its
repository, so the fact is stored once, and a repository transfer moves the run's history with it
(ADR 0002). `migrations/0004`'s `CHECK (repository_id IS NULL OR organization_id IS NULL)` stays.

This corrects the schema consequence above, which says `run.organization_id` is "null only on the
dev-only unauthenticated path". It is also null on every attributed run:

```
run
  id
  organization_id      NULL       -- set only on an unattributed run whose organization is known
  repository_id        NULL       -- null = unattributed (ADR 0002)
  api_token_id         NULL       -- the token that uploaded it; null for OIDC
  ...
  CHECK (repository_id IS NULL OR organization_id IS NULL)   -- migrations/0004
```

An unattributed run's organization, when it has one, comes from the token, or from the GitHub App
installation linked to the account named in the OIDC token's `repository_owner_id` claim.

### 2. Not every stored run has an organization

This corrects the consequence above, "Every stored run has an authenticated organization". An OIDC
upload from an unmonitored repository, whose owner has no linked installation, may be stored with no
organization.

What holds is narrower: when a run has an organization, it was authenticated. It comes from the
run's repository, from the token, or from the installation linked to the OIDC token's owner, and
never from a header the caller chose. Two kinds of run have none:

- runs from the dev-only unauthenticated path (section 3);
- OIDC uploads from an unmonitored repository whose owner has no linked installation.

No tenant sees a run with no organization, and ADR 0002's count of unattributed runs per
organization doesn't include it. An OIDC upload keeps its owner claim, so its organization can be
derived once an installation is linked.

### 3. The dev-only path, and no identity headers

The dev-only unauthenticated path stays behind `KOVALLENT_ALLOW_UNAUTHENTICATED_INGEST`, off by
default, and stores runs with no organization and no repository.

`X-Kovallent-Provider-Repo-Id` is removed, as `X-Kovallent-Organization-Id` is (decision 7). No
header names an organization or a repository on any path, so an unauthenticated run can't be
attributed to either.

### 4. A token has a name

```
api_token
  ...                             -- as in the schema consequence above, plus:
  name                 text NOT NULL   -- given by the person when they make the token
```

The name and `display_prefix` are how people tell their tokens apart.

### 5. 401, 403 and 404 for tokens

A refusal uses one of three codes, checked in this order:

- **401** for a missing, malformed, expired or revoked token, and for a token whose member is
  deactivated. This matches sessions, which get 401 once every one of the signed-in person's
  memberships is deactivated; a token belongs to only one.
- **403** for a token that lacks the scope the request needs: `read` for any `/v1/` read, `upload`
  for posting a run. The scope is checked before anything the request names is looked up, so the
  response is the same for every id, whether it exists or not. A rejected upload is not stored.
- **404** for anything else the token can't read, including other organizations' resources and ids
  that don't exist, the same as a session. A response never shows whether something exists outside
  what the token can read.

### 6. Only OIDC uploads count as scans

Only OIDC uploads update `repository.last_scanned_at`, the column ADR 0002's quota check counts. A
token upload never does, even when it's attributed, so a local run can't make a repository count
against the quota (ADR 0002).
