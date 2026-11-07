## 0.36.0

Unreleased

### Changed

- `omnia_dapr::axum::subscriptions` takes the guest's
  `omnia_sdk::api::Router<P>`, the router `Handlers::with_state` closes,
  in place of the removed `omnia_sdk::api::messaging::Router<P>`. Its topics
  are registered through `Handlers::topic` and `Handlers::topic_with` with
  context-first handlers (`async fn(Context<P>, I) -> Result<O, E>`).

---

Release notes for previous releases can be found on the respective release branches of the repository.

<!-- ARCHIVE_START -->
* [0.35.x](https://github.com/augentic/omnia-ext/blob/release-0.35.0/RELEASES.md)
* [0.34.x](https://github.com/augentic/omnia-ext/blob/release-0.34.0/RELEASES.md)
