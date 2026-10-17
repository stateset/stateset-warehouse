# WorkOS authentication

WorkOS AuthKit is implemented as an optional sign-in provider. It is **not enabled
in a deployment merely by building this repository**. No real WorkOS project,
custom domain, organization mapping or membership lifecycle has been qualified.
The existing API-key mode and loopback synthetic demo remain available when
`WMS_WORKOS_JSON` is absent. Configured WorkOS replaces browser credential login;
independent service API keys continue to work on the API.

## Configure a project

Use the WorkOS AuthKit application intended for WMS. Configure its production
Authentication API custom domain under the same site as the warehouse browser,
for example `auth.example.com` and `warehouse.example.com`. This permits the
SDK's secure refresh cookie and session-presence cookie to operate together.
Add the exact warehouse origin to allowed CORS origins, and the exact
`https://warehouse.example.com/` to redirect and sign-out URIs. Sign-in always
returns to `/`; external return destinations from URL/query/OAuth state are
ignored. `/login` displays the same explicit sign-in action.

Set the WorkOS access-token lifetime to **300 seconds or less**. Require membership
in an approved organization and create explicit warehouse role slugs; do not
assign a general application `member` role administrator access. Configure SSO
and MFA requirements in WorkOS according to the organization's access policy.
WMS does not administer identity-provider policies or provision memberships.

Configure the server, using actual IDs and tenant names:

```dotenv
WMS_DEMO=false
WMS_WORKOS_JSON={"client_id":"client_REPLACE","api_origin":"https://auth.example.com","issuer":"https://auth.example.com/","organizations":{"org_REPLACE":{"tenant":"your-org","roles":{"warehouse-viewer":"viewer","warehouse-operator":"operator","warehouse-admin":"admin"}}}}
```

`api_origin` is a canonical HTTPS origin without a trailing slash, credentials,
port, path or query. `issuer` is that same origin **with** a trailing slash.
The signing endpoint is derived as `/sso/jwks/<client_id>`; there is no arbitrary
JWKS override. For a WorkOS environment using its default API domain these
values are `https://api.workos.com` and `https://api.workos.com/`, respectively;
production browser cookie behavior still requires the same-site custom domain.
The frontend receives only provider, client ID and hostname from public,
`no-store` `/auth/config`. Tenant and role maps remain server-side. CSP permits
connections to that one configured authentication origin in addition to self.
Vite proxies `/auth` to the backend, so the browser uses the same runtime config
in development and the built image.

Each organization must map to a distinct non-demo tenant. Only explicitly
mapped `viewer`, `operator` and `admin` roles are accepted. WorkOS membership
roles never grant `oms` service permissions. Actor attribution is stable
`workos:<verified user ID>`, independent of email or refreshed session ID.
API-key actors cannot use this reserved prefix. Missing service-key configuration
is allowed when WorkOS is configured; without either provider real mode refuses
to start. Demo mode rejects WorkOS. Malformed/partial WorkOS configuration fails
startup rather than silently enabling another login mode.

Deploy this release to every API replica before enabling WorkOS; older API-key-only replicas cannot verify these sessions.

Compose inherits `WMS_WORKOS_JSON` when present. Set `WMS_API_KEYS_JSON` only for
needed service identities. Remove or rotate legacy human API keys during the cutover: WorkOS offboarding does not revoke a separately configured static credential. Verify that retired keys return 401. Restart every WMS instance after changing mappings.
Mapped tenants are provisioned with the same empty MAIN warehouse used by API-key
mode. Unverified organization IDs from a browser never create or select a tenant.

## Verification and recovery boundaries

The backend accepts RS256 only and verifies the signature, configured issuer,
expiry and optional not-before timestamp. User, session, organization, role and
issued-at claims must be present. Tokens with future issuance beyond 30 seconds,
lifetimes over 300 seconds, more than 300 seconds remaining, impersonation, or
unmapped membership are rejected. Normal AuthKit session tokens omit audience;
with no `audience` config, tokens carrying `aud` are rejected. If your project
issues an audience, set `"audience":"exact-expected-value"`; it becomes required
on every token. This is AuthKit session verification, not WorkOS Connect resource
token verification. Roles come from the verified singular membership `role`,
not arbitrary permissions arrays or client headers.

Signing keys are cached for five minutes. An unknown key can trigger a refresh
at most once per 30 seconds; concurrent fetches share one bounded request.
Responses are limited to 64 KiB and 32 keys, redirects are refused, and duplicate
key IDs are rejected. Key fetches time out after three seconds; authentication
has a four-second outer deadline. A fresh cached key remains usable during a
provider outage. Once the cache expires, verification fails closed with 503;
old keys are not used indefinitely. A rotated key may be temporarily rejected
during the 30-second refresh cooldown. APIs return generic errors without tokens,
provider response bodies or configuration internals.

The real AuthKit React SDK handles PKCE, refresh cookies and provider sign-out.
`devMode` is explicitly false, including on localhost: tokens are never persisted
by WMS or SDK development-mode storage. Local browser qualification intercepts
provider HTTP; it does not weaken the production verifier or add a fake-login
route. Use a qualified HTTPS same-site deployment to exercise actual cookies.

Every warehouse request obtains a current access token. The existing 20-second request deadline also covers token acquisition; a late refresh cannot resume an aborted save. A refreshed token must
still match the view's user, organization and role before any request is sent.
Changing that scope destroys the old view and aborts its pending fetches. Session
expiry shows sign-in again instead of sending an automatic replacement command.
Pending command journals remain scoped by verified tenant and actor in session
storage. Sign-out removes credentials and local views, **not unresolved command
identity**. After returning as the original operator, check the original save
before repeating physical work. Browser profile/tab loss can still remove this
journal; use the retained server ledger and incident process in that situation.

Membership removal and provider sign-out prevent future refresh, but a previously
issued signed token can remain valid until its expiry (at most five minutes).
There is no instant session-revocation webhook or per-request WorkOS membership
lookup. A request already authorized may finish after sign-out. Removing a server
mapping and restarting all instances denies that organization's subsequent calls.
Removing a signing key is observed on cache refresh. Keep these bounds explicit
in access-removal and incident procedures.

## Qualification

Rust tests use real RSA signatures and a local JWKS server to exercise forgery,
issuer/audience/time checks, missing/unmapped memberships, impersonation denial,
key rotation, cache expiry/outage, coalescing, response limits and HTTP role guards.
The PostgreSQL test submits signed-token requests through the real router and
proves persisted attribution, same-user session refresh replay, other-user receipt
isolation and cross-organization isolation. Public test private keys under
`tests/fixtures` are never deployed as trust roots.

Browser tests run the built application and actual AuthKit SDK against intercepted
provider HTTP. They cover PKCE redirect, sign-in/out, token storage absence,
configuration failure, refresh and pending-save isolation. They do not establish
actual project CORS, cookies, SSO/MFA, provisioning or offboarding.

Before enabling operator traffic, record the deployed image digest, exact WorkOS
client/domain, approved organization/tenant/role mapping, successful sign-in/out,
refresh across token expiry, rejected unmapped organization and role, removal of
membership followed by rejection after expiry, and lost-response recovery by the
same actor. Verify those observations in the actual browser and IdP with the
organization's identity administrator. Keep credentials in the secret manager.

References: [WorkOS sessions](https://workos.com/docs/authkit/sessions) and the
[official AuthKit React SDK](https://github.com/workos/authkit-react).
