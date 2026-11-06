//! `Cache-Control` and `If-None-Match` request header parsing.

use anyhow::{Context, Result, bail};
use http::HeaderMap;
use http::header::{CACHE_CONTROL, IF_NONE_MATCH};

/// Caching directives parsed from a request's `Cache-Control` and
/// `If-None-Match` headers.
#[derive(Clone, Debug, Default)]
pub struct Control {
    // If true, bypass any stored copy and make the HTTP request. Per RFC 9111
    // §5.2.1.4 the request directive only forbids serving a stored response
    // without validation; whether the new response is written back still
    // depends on `max_age` supplying a lifetime.
    no_cache: bool,

    // If true, make the HTTP request and do not cache the response.
    no_store: bool,

    // Length of time to cache the response in seconds. Zero (also the value
    // when the directive is absent) disables the store in both directions:
    // RFC 9111 §5.2.1.1 defines a request `max-age` as the oldest response
    // the client will accept, so `max-age=0` cannot be satisfied from the
    // store and there is no lifetime to write a fresh copy under.
    max_age: u64,

    // ETag to use as the cache key, derived from the `If-None-Match` header.
    etag: String,
}

impl Control {
    /// Parse the caching directives when `Cache-Control` carries at least one
    /// this cache acts on: `no-store`, `no-cache` or `max-age`.
    ///
    /// Returns `None` when the header is absent or carries only directives
    /// this cache does not recognise: RFC 9111 §5.2.3 requires a cache to
    /// ignore those, so the request passes through as if none were present.
    ///
    /// # Errors
    ///
    /// Returns an error if the directives are malformed or conflict.
    pub fn maybe_from(headers: &HeaderMap) -> Result<Option<Self>> {
        if headers.get(CACHE_CONTROL).is_none() {
            tracing::debug!("no Cache-Control header present");
            return Ok(None);
        }
        Self::parse(headers).context("issue parsing Cache-Control headers")
    }

    /// Whether a stored response may be served without contacting the origin.
    ///
    /// Requires a positive `max-age`: `max-age=0` asks for a response no
    /// older than zero seconds, which only the origin can provide.
    #[must_use]
    pub const fn reads(&self) -> bool {
        !self.no_cache && !self.no_store && self.max_age > 0 && !self.etag.is_empty()
    }

    /// Whether a successful origin response should be stored.
    ///
    /// `no-cache` on its own does not write: it bypasses the stored copy but
    /// supplies no lifetime, so only `no-cache, max-age=<secs>` refreshes it.
    #[must_use]
    pub const fn writes(&self) -> bool {
        !self.no_store && self.max_age > 0 && !self.etag.is_empty()
    }

    /// The raw `If-None-Match` value, doubling as the storage key.
    #[must_use]
    pub fn etag(&self) -> &str {
        &self.etag
    }

    /// The `max-age` directive in seconds (zero when absent).
    #[must_use]
    pub const fn max_age(&self) -> u64 {
        self.max_age
    }
}

impl Control {
    fn parse(headers: &HeaderMap) -> Result<Option<Self>> {
        let mut control = Self::default();
        // `Some` whenever the directive appeared, so `max-age=0` still counts
        // as combining it with `no-store`.
        let mut max_age = None;

        // `Cache-Control` is a list field (RFC 9111 §5.2), so several field
        // lines are equivalent to one comma-joined line (RFC 9110 §5.3):
        // every line is read, and the directives form a set whose order
        // carries no meaning.
        for line in headers.get_all(CACHE_CONTROL) {
            for directive in line.to_str()?.split(',') {
                let directive = directive.trim().to_ascii_lowercase();

                if directive == "no-store" {
                    control.no_store = true;
                } else if directive == "no-cache" {
                    control.no_cache = true;
                } else if let Some(value) = directive.strip_prefix("max-age=") {
                    // RFC 9111 §4.2.1: a repeated directive is either taken
                    // from its first occurrence or treated as invalid. A
                    // caller sending two lifetimes has made a mistake worth
                    // surfacing, so it is invalid here.
                    if max_age.is_some() {
                        bail!("`max-age` directive given more than once");
                    }
                    let Ok(secs) = value.trim().parse() else {
                        bail!("`max-age` directive is malformed");
                    };
                    max_age = Some(secs);
                }

                // RFC 9111 §5.2.3: unrecognised directives (and empty list
                // elements) are ignored.
            }
        }

        // Nothing this cache acts on: RFC 9111 §5.2.3 says ignore the rest,
        // so the request is treated as if it carried no `Cache-Control`.
        if !control.no_store && !control.no_cache && max_age.is_none() {
            tracing::debug!("no recognised Cache-Control directive present");
            return Ok(None);
        }

        // Conflicts are judged on the whole set, so `max-age=0, no-store` and
        // `no-store, max-age=0` are refused alike.
        if control.no_store && (control.no_cache || max_age.is_some()) {
            bail!("`no-store` cannot be combined with `no-cache` or `max-age`");
        }
        control.max_age = max_age.unwrap_or(0);

        // `no-cache` refreshes the stored copy whenever `max-age` accompanies
        // it, and every cached-path response is stamped with the etag, so it
        // needs the key too; only `no-store` can do without one.
        if !control.no_store {
            // `If-None-Match` is a list field too (`#entity-tag`), so a second
            // line is a second etag, refused exactly like a comma.
            let mut lines = headers.get_all(IF_NONE_MATCH).iter();
            let Some(etag) = lines.next() else {
                bail!(
                    "`If-None-Match` header required when using `Cache-Control: max-age` or `no-cache`"
                );
            };
            if lines.next().is_some() {
                bail!("multiple `etag` values in `If-None-Match` header are not supported");
            }
            // The etag doubles as the `&str` store key, so `obs-text` octets
            // (RFC 9110 §5.5), though grammatically valid, are not accepted.
            let Ok(etag) = etag.to_str() else {
                bail!("`If-None-Match` contains `obs-text` octets, which cannot form a cache key");
            };
            control.etag = strong_etag(etag)?.to_string();
        }

        Ok(Some(control))
    }
}

/// Validate `value` as exactly one strong entity-tag and return it trimmed of
/// surrounding whitespace.
///
/// RFC 9110 §8.8.3: `entity-tag = [ weak ] opaque-tag`, `opaque-tag = DQUOTE
/// *etagc DQUOTE`, `etagc = %x21 / %x23-7E / obs-text`. A comma is a legal
/// `etagc`, so a list is detected by finding content after the closing quote,
/// not by looking for commas. `obs-text` is excluded by the caller, which
/// needs the tag as a `&str`, so only the ASCII range is checked here.
fn strong_etag(value: &str) -> Result<&str> {
    let value = value.trim_matches([' ', '\t']);
    if value.is_empty() {
        bail!("`If-None-Match` header is empty");
    }
    if value == "*" {
        bail!("`If-None-Match: *` is not a usable cache key");
    }
    if value.starts_with("W/") {
        bail!("weak `etag` values in `If-None-Match` header are not supported");
    }
    let Some(body) = value.strip_prefix('"') else {
        bail!("`If-None-Match` value is not a quoted entity-tag");
    };
    // `etagc` excludes DQUOTE, so the first one closes the tag.
    let Some(close) = body.find('"') else {
        bail!("`If-None-Match` entity-tag is missing its closing quote");
    };
    let (opaque, rest) = body.split_at(close);
    if !rest[1..].trim_matches([' ', '\t']).is_empty() {
        bail!("multiple `etag` values in `If-None-Match` header are not supported");
    }
    if !opaque.bytes().all(|b| b == 0x21 || (0x23..=0x7E).contains(&b)) {
        bail!("`If-None-Match` entity-tag contains characters outside `etagc`");
    }
    Ok(value)
}
