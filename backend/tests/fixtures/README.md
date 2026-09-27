`oidc-test-only.pem` is a deliberately public RSA test key. It signs offline OIDC
fixtures only. It is never loaded by the running backend and must never be used
for a deployment or real credentials.
