# omnia-http-cache

An HTTP response cache for Omnia guests. `HttpCache` decorates any
`omnia_sdk::HttpClient` so that requests carrying `Cache-Control` and
`If-None-Match` are answered from, and written back through, an
`omnia_sdk::StateStore`. Caching is opt-in per call site: wrap the provider
where cached fetches are wanted and use the bare provider everywhere else.

## Header contract

- A request without `Cache-Control`, or whose `Cache-Control` carries none of
  the three directives below, passes through untouched: RFC 9111 §5.2.3 has a
  cache ignore directives it does not recognise, so `no-transform` or an
  extension alone neither needs an etag nor has its validators rewritten.
- `Cache-Control` directives: `max-age=<secs>` (serve a stored response, or
  store a successful one for that long), `no-cache` (bypass the stored copy
  and contact the origin), `no-store` (contact the origin and store nothing).
  `no-store` cannot be combined with the other two, in any order and whatever
  the `max-age` value, and `max-age` may appear only once (RFC 9111 §4.2.1
  allows a repeated directive to be treated as invalid). Both headers are list
  fields, so repeated field lines are read as one comma-joined list (RFC 9110
  §5.3): a second `Cache-Control` line adds directives, a second
  `If-None-Match` line is a second etag and is refused.
- `max-age` is the only lifetime the cache knows, so it gates the store in
  both directions. `max-age=0` neither serves nor stores: per RFC 9111
  §5.2.1.1 a request `max-age` is the oldest response the client will accept,
  which makes zero a revalidation. Likewise `no-cache` alone only bypasses
  (RFC 9111 §5.2.1.4 says nothing about storing); `no-cache, max-age=<secs>`
  is the forced refresh that replaces the stored copy.
- `max-age` and `no-cache` require `If-None-Match` carrying a single strong
  etag, validated against the RFC 9110 §8.8.3 grammar: weak (`W/`) tags,
  `*`, bare tokens and lists are refused before any request leaves. A comma
  inside the quotes is legal `etagc`, so `"v1,v2"` is one etag. `obs-text`
  octets (0x80–0xFF) are grammatically valid but refused, because
  `Control::etag` hands the tag out as a `&str`; RFC 9110 §5.5 asks new
  senders to stay within visible ASCII anyway.
- Only complete 2xx responses are stored; a `206 Partial Content`, 304 or 5xx
  is returned but never cached. RFC 9111 §3 reserves storing a 206 for caches
  that do the range bookkeeping of §3.3–§3.4, which this one does not.
- `If-None-Match` is not forwarded to the origin: the cache owns conditional
  semantics, so the origin always answers with a full body.
- The request's etag is written onto the response `ETag` on every cached-path
  response (hit or miss), replacing whatever the origin sent. `no-store`
  carries no request etag, so the origin's `ETag` passes through unchanged.
- The storage key is `store_key(etag)`: `http-cache.` followed by the
  lower-case hex SHA-256 of the `If-None-Match` entity-tag as sent, quotes
  included. It is a portable `StateStore` key whatever the tag holds, so a
  backend that refuses `"` or `:` in a key still caches. The injected
  `StateStore` decides where entries live and the TTL is `max-age`. Header
  values are stored as raw bytes, so `obs-text` (RFC 9110 §5.5) survives a
  round trip.
- The store never fails a request. A read error or an entry that no longer
  deserializes is treated as a miss; a write error hands back the origin
  response uncached. Both are logged at `warn`. Malformed `Cache-Control` is
  the caller's error and fails with `ErrorKind::Invalid` before any request
  leaves.

## Usage

On `wasm32`, `omnia_sdk::WasiProvider` implements all capability traits over
WASI imports, so it can serve as both transport and store:

```rust,ignore
use omnia_http_cache::HttpCache;
use omnia_sdk::{HttpClient, WasiProvider};

let cache = HttpCache::new(WasiProvider, WasiProvider);
let request = http::Request::get("https://origin.example/resource")
    .header(http::header::CACHE_CONTROL, "max-age=300")
    .header(http::header::IF_NONE_MATCH, "\"v1\"")
    .body(bytes::Bytes::new())?;
let response = cache.fetch(request).await?;
```

Natively, wrap an `omnia_test::guest::Provider` (or any other `HttpClient`
and `StateStore` implementations) the same way; `&P` implements both traits,
so `HttpCache::new(&provider, &provider)` shares one provider.
