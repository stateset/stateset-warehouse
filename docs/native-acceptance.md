# Deployed OMS and Freight acceptance

`scripts/qualify-native.py preflight` checks real, explicitly selected services using GET requests. It never creates orders, inventory, loads or fulfillment receipts. Missing recipients, credentials, capabilities, signed mapping evidence or successful checks produce `result: "unqualified"` and exit status 2. A successful read-only preflight returns 0; `production_qualified` remains false because a read does not establish a successful shipment or physical facility trial.

The [existing native contract](native-integrations.md) remains authoritative. WMS owns physical custody, OMS owns commercial order state, and Freight owns transportation. Deploy the [companion receiver](../integrations/stateset-sync-server/README.md) through the OMS release process before selecting `conditional_v1`. This repository does not apply changes to another repository or deploy that receiver automatically.

## Configure the selected deployment

Use Python 3.8 or newer and OpenSSL. Put this nonsecret configuration in a restricted working directory outside the repository. Replace example origins and identities with actual staging values. The selected pending/processing source must appear on the first native page, have exact whole-each quantities and remain unchanged during the run. Create or select that fixture through the approved OMS process before running this read-only tool.

```json
{
  "schema_version": 1,
  "environment": "staging",
  "warehouse_id": "wh-main",
  "source_order_id": "00000000-0000-0000-0000-000000000001",
  "foreign_oms_tenant": "existing-foreign-oms-tenant",
  "wms": {
    "base_url": "https://wms.staging.example.com",
    "tenant": "warehouse-tenant",
    "credential_env": "WMS_QUALIFY_OPERATOR",
    "viewer_credential_env": "WMS_QUALIFY_VIEWER",
    "foreign_credential_env": "WMS_QUALIFY_FOREIGN"
  },
  "oms": {
    "base_url": "https://oms.staging.example.com",
    "tenant": "oms-tenant",
    "credential_env": "WMS_QUALIFY_OMS"
  },
  "freight": {
    "base_url": "https://freight.staging.example.com",
    "tenant": "freight-tenant",
    "credential_env": "WMS_QUALIFY_FREIGHT"
  }
}
```

Supply secrets using the named environment variables from the deployment secret manager. Operator, viewer and foreign WMS credentials must differ. The foreign credential must resolve to a different WMS tenant. Both tenants may correctly have their own `wh-main`; the check positively resolves one existing opaque order/receipt UUID under the target operator, then requires that same record to return 404 under the foreign credential. An empty target needs a controlled fixture created through its normal process before this read-only proof can pass. `foreign_oms_tenant` must be an existing tenant to prove denial rather than the absence of a path. Its denial must be 401 or 403, not an unknown-tenant 404. Select a WMS operator/admin, a WMS viewer, an OMS tenant key with `orders:read` and `orders:write`, and a Freight credential with `operations.write`. Do not use platform admin bypass credentials.

Remote origins require HTTPS with normal certificate validation. The tool refuses URL credentials, queries, fragments and path prefixes; disables redirects and ambient proxies; bounds responses to 2 MiB; uses eight-second request timeouts; and never emits credentials or source order bodies. `--allow-loopback` permits a deliberate local service/tunnel run and labels it `local_or_tunnel`. Fixture tests and loopback tests do not establish external deployment qualification.

## Attest the private recipient mapping

The current public APIs do not expose Freight's private `sync_tenant_id`/`sync_url`, the WMS's private recipient URLs, or deployed image digests. Comparing matching source IDs cannot establish those configuration facts. A release operator must inspect the running deployment configuration and immutable image identities, then produce a signed manifest with this structure:

```json
{
  "schema_version": 1,
  "environment": "staging",
  "observed_at": "2026-10-17T16:00:00Z",
  "signer": "Named release operator",
  "signer_role": "release_operator",
  "services": {
    "wms": {
      "base_url": "https://wms.staging.example.com",
      "tenant": "warehouse-tenant",
      "source_sha": "0000000000000000000000000000000000000000",
      "image_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
    },
    "oms": {
      "base_url": "https://oms.staging.example.com",
      "tenant": "oms-tenant",
      "source_sha": "0000000000000000000000000000000000000000",
      "image_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
    },
    "freight": {
      "base_url": "https://freight.staging.example.com",
      "tenant": "freight-tenant",
      "source_sha": "0000000000000000000000000000000000000000",
      "image_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
    }
  },
  "recipient_mapping": {
    "wms_oms_url": "https://oms.staging.example.com",
    "wms_oms_tenant": "oms-tenant",
    "wms_freight_url": "https://freight.staging.example.com",
    "wms_freight_tenant": "freight-tenant",
    "freight_oms_url": "https://oms.staging.example.com",
    "freight_oms_tenant": "oms-tenant"
  },
  "oms_credential_scopes": ["orders:read", "orders:write"]
}
```

Replace every placeholder with the observed release and configuration. The manifest must be at most 24 hours old; five minutes of future clock skew is allowed. Sign the exact JSON bytes using the facility/release team's existing RSA or ECDSA signing key. The accepting operator obtains the public key through an independently trusted channel. A key supplied by the evidence author alone does not establish that author's authority. Keep the private key outside this repository.

```bash
openssl dgst -sha256 -sign /secure/release-signing-private.pem \
  -out /secure/qualification/deployment.sig /secure/qualification/deployment.json
python3 scripts/qualify-native.py preflight \
  --config /secure/qualification/targets.json \
  --deployment-manifest /secure/qualification/deployment.json \
  --deployment-signature /secure/qualification/deployment.sig \
  --trust-key /secure/trust/release-signing-public.pem \
  --output /secure/qualification/native-preflight.json
```

The report stores the trusted public key's DER SHA256 fingerprint and separates signed operator assertions from live HTTP observations. Signature validation authenticates the supplied key; it does not independently inspect orchestrator state or prove a private mapping. Preserve the original manifest, signature and deployment evidence for review.

## Executed preflight checks

| Check | Required observation |
|---|---|
| Authentication and tenancy | Each service rejects absent/invalid credentials; OMS rejects the key in another existing tenant; WMS viewer cannot inspect operator-only native work; a different authenticated WMS tenant cannot resolve a positively verified target order/receipt UUID |
| WMS deployment | Database readiness, `demo: false`, correct operator/admin tenant, warehouse visibility, configured Freight and opt-in conditional OMS |
| OMS receiver | GET `/v1/tenants/{tenant}/warehouse-fulfillments` reports the exact contract/tenant plus all four true guarantees: conditional revision, terminal state fence, durable idempotency and durable rejection |
| Freight identity | GET `/v1/access` selects the intended Freight tenant and reports `operations.write` |
| Physical ledger | Full append-only ledger/order-allocation reconciliation succeeds, including each returned stock and receipt proof |
| Source continuity | Direct OMS source before/after, WMS native list and Freight native list agree on source UUID, pending/processing status, nanosecond revision and exact independent line identity/SKU/whole-each quantities |
| Recipient mapping | Recent independently signed deployment observation selects the same OMS authority in WMS and Freight and records each service's immutable image/source identities |

Matching source content provides useful corroboration, not proof of hidden tenant mappings. The source projection deliberately compares fulfillment-relevant fields rather than customer or price data. Selection outside page one, a concurrent source revision, missing facts or an unsupported API fails the preflight; review the target rather than weakening the assertions.

## Controlled staging lifecycle still required

The read-only tool deliberately does not arm a remote mutation. For explicitly approved synthetic fixtures in a selected staging tenant, capture the following through the actual UI/API and the existing [facility evidence validator](facility-acceptance.md):

1. Import one complete OMS order, including two independent lines sharing a SKU. Receive, put away, allocate, pick and pack exact physical quantities. Verify one native source owner across warehouse scopes.
2. Create one Freight planned load from the packed order. Replay the original WMS request and verify one remote load and the same receipt. Check Freight's source review and confirm no pickup/delivery event was inferred.
3. Record an actual handoff, then deliver its conditional OMS request. Verify the receiver receipt's tenant, order, request, physical event and digest, and the single commercial fulfillment effect.
4. Interrupt the response after remote commit, reload/retry the original retained request and prove a single effect. Change the source only after preserving the uncertain request, and verify retry uses the original body, authority and key.
5. Use separate fixtures for cancellation/revision before acceptance and partial/terminal source states. Verify durable rejection, explicit reviewed replacement and immutable old-rejection replay. A timeout, raw conflict or malformed response must remain uncertain.
6. Rotate a credential inside the same tenant; separately test changed recipient/tenant and insufficient roles. Retained uncertain intents must not move to another authority.

Keep tenant-safe request IDs, remote receipts, audit exports, immutable source/image identities and independent stock reconciliation. Label the lifecycle `not_performed` until those real-service observations exist. Current automated receiver/HTTP/PostgreSQL checks are strong implementation evidence; they do not substitute for this deployed sequence.

Harness regression:

```bash
python3 -m unittest discover -s scripts -p 'test_qualify_native.py' -v
```

The tests use explicitly simulated HTTP, temporary signing keys and invented observations. They verify denial, capability, source revision, signature and evidence guards, and never issue a production qualification.
