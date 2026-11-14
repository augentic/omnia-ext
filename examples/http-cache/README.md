# HTTP Cache Example

Demonstrates caching outbound HTTP responses with `omnia-http-cache`.

This example shows how to:

- Wrap the SDK's `HttpClient` capability in `HttpCache` at the call site
- Store responses through `wasi:keyvalue` using the SDK's `StateStore`
- Drive the cache with `Cache-Control: max-age` and `If-None-Match`

The guest serves `GET /cached`, which fetches `{UPSTREAM_URL}/resource`
through the cache. The first request reaches the origin and the response is
stored for 300 seconds; every request after that is answered from the store.

## Quick Start

`UPSTREAM_URL` is read from the runtime's environment via `wasi:config`. Any
origin that answers `GET /resource` will do:

```bash
make build http-cache
UPSTREAM_URL=https://jsonplaceholder.cypress.io/posts/1 make run http-cache
```

Or, more manually, for debugging:

```bash
# build the guest
cargo build -p examples --example http-cache-wasm --target wasm32-wasip2

# run the host
export UPSTREAM_URL=https://jsonplaceholder.cypress.io/posts/1
export RUST_LOG="info,opentelemetry_sdk=off,omnia_wasi_http=debug,omnia_http_cache=debug,http_cache=debug"
cargo run -p examples --example http-cache -- run ./target/wasm32-wasip2/debug/examples/http_cache_wasm.wasm
```

## Test

```bash
# first call: origin hit, response stored under the etag's store_key
curl -i http://localhost:8080/cached

# second call: served from the store; no upstream request is logged
curl -i http://localhost:8080/cached
```

Both responses carry `ETag: "v1"`, the request etag stamped by the cache;
the `omnia_http_cache=debug` log line shows `cache hit` on the second call
while `omnia_wasi_http` logs only one outbound request.

The header contract (directives, the strong single-etag requirement, what is
and is not stored) is documented in the
[`omnia-http-cache` README](../../crates/http-cache/README.md).
