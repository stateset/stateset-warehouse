The WorkOS RSA keys in this directory are **public test fixtures**, generated
only for local JWT/JWKS tests. They have never been deployed or registered with
WorkOS. Anyone can sign with them: never trust them outside the test fixture.
The production verifier fetches only the configured client's HTTPS JWKS.
