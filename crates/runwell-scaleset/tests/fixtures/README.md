Synthetic fixtures independently assembled from `actions/scaleset` e6daac7
`types.go` and the wire shapes/status expectations in `client_test.go`,
`session_client_test.go`, and `errors_test.go`. No Rust-port code or private
workload records were copied. Queue URLs, names, IDs, dates and secrets are
synthetic. `message.json` deliberately contains a JSON string holding a JSON
array, valid message ID zero, and both Go zero-time encodings.

The App PEM key pair was generated solely for local JWT signature tests. It has
no service installation or production use.

`app-test-key.pem` / `app-test-public.pem` are a throwaway RSA key pair generated only to sign and verify
GitHub App JWTs in `tests/auth.rs`. They are not credentials for anything.
