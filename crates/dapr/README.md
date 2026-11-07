# omnia-dapr

Dapr HTTP v1.0 sidecar provider and `CloudEvents` 1.0 subscription bridge for
[Omnia](https://github.com/augentic/omnia) guest handlers.

`Dapr<H>` implements the `omnia_sdk` capability traits that Dapr's HTTP v1.0
building blocks support (`StateStore`, `Publish`, `Config`, and basic
`BlobStore` object operations) over any `H: HttpRequest`. Inside a
`wasm32-wasip2` guest, `Dapr<WasiProvider>` routes Dapr building blocks over
`wasi:http` while `#[derive(omnia_sdk::Provider)]` delegates any remaining
capabilities to `WasiProvider`.

## Capabilities and contract deviations

| Trait | Dapr building block | Notes |
| --- | --- | --- |
| `StateStore` | State API (`/v1.0/state/{store}`) | Raw bytes are base64-encoded in the JSON state item so binary payloads and 8-byte big-endian counters round-trip intact. `set` sends `metadata.ttlInSeconds` in one round trip. `cas` reads the current value and `ETag`, then issues a `first-write` conditional `POST` re-supplying any remaining TTL tracked in-memory on this `Dapr` instance (Dapr's state `GET` does not return a key's remaining TTL). **Deviation:** `increment` uses a bounded CAS retry loop over an 8-byte big-endian `i64` because Dapr has no atomic counter API. |
| `Publish` | Pub/Sub API (`/v1.0/publish/{pubsub}/{topic}`) | Payload is sent as the request body; `Message::headers` map to `metadata.*` query parameters. |
| `Config` | Configuration API (`/v1.0/configuration/{store}`) | Optional process-environment fallback for local runs. |
| `BlobStore` (partial) | Output bindings (`/v1.0/bindings/{container}`) | Serves `get`, `put`, `delete`, and `list`; `get_range`, `object_info`, and container operations return `Unsupported`. |

Capabilities outside Dapr's building blocks (`TableStore`, `DocumentStore`,
`Identity`, `Vault`, `Broadcast`, `Model`, `Vcs`, `Plugins`) are served
alongside `Dapr<H>` by `WasiProvider` on a `#[derive(omnia_sdk::Provider)]`
struct.

## Inbound subscriptions

`omnia_dapr::axum::subscriptions(router, &dapr)` takes the guest's
`omnia_sdk::api::Router<P>` (its topics registered through
`Handlers::topic` and closed with `Handlers::with_state`) and builds an
`axum::Router` serving:

- `GET /dapr/subscribe` — lists every topic from `router.topics()`.
- `POST /dapr/events/{*topic}` — decodes structured or binary `CloudEvents` 1.0
  into an `omnia_sdk::api::messaging::Delivery`, dispatches it through
  `router.handle`, and returns Dapr's `{"status": "SUCCESS" | "RETRY" | "DROP"}`
  acknowledgement.
