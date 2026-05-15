# S3 Backend Spike

Date: 2026-05-15

## Goal

Choose the first dependency for an OmniFuse S3-compatible backend.

The dependency must fit the existing `Backend` contract: OmniFuse core only
tracks local dirty paths and schedules refreshes; the backend owns remote state
tracking, merge decisions, and conflict-safe writes.

For text merge, the hard requirement is compare-and-swap style writes:

- read or stat returns an ETag or equivalent remote version;
- write can be conditional on the previously observed ETag;
- failed preconditions are surfaced as a distinct, retryable conflict signal.

The current text merge decision helper lives in `omnifuse-wiki`. The S3 backend
must not depend on the wiki crate for domain logic; move the reusable merge
primitive into `omnifuse-core` or a small shared crate before implementing the
S3 sync session.

## Shortlist

| Candidate | Current version checked | Result |
|---|---:|---|
| `object_store` | `0.13.2` | Recommended first spike |
| `opendal` | `0.56.0` | Strong fallback when broad storage abstraction matters more |
| `aws-sdk-s3` | `1.132.0` | Compatibility fallback, not the default dependency |
| `s3` | `0.1.27` resolved locally | Demoted: high-level `PutObjectRequest` lacks conditional write setters |

## Findings

### `object_store`

`object_store` is the best fit for the first OmniFuse S3 backend.

Why:

- It models object storage directly instead of generic storage.
- Conditional writes are first-class through `PutMode::Create` and
  `PutMode::Update(UpdateVersion)`.
- S3 conditional put defaults to ETag matching.
- It normalizes S3 precondition failures into `Error::Precondition`.
- It provides memory/local stores for backend logic tests without a real S3
  service.

Compile-checked shape:

```rust
let mode = PutMode::Update(UpdateVersion {
  e_tag: Some(etag),
  version
});

store.put_opts(path, bytes.into(), mode.into()).await?;
```

Tradeoff:

- It is an object-store abstraction, not an S3 SDK. If the backend later needs
  deep S3-only features, use `aws-sdk-s3` behind a narrower adapter.

### `opendal`

`opendal` remains a strong candidate, especially if the product goal is a broad
storage abstraction rather than just S3-compatible object stores.

Why:

- S3 support exposes `write_with_if_match` and `write_with_if_not_exists`.
- Metadata includes ETag and optional version fields.
- The S3 service exposes compatibility knobs for R2-like behavior.
- Feature-gated service crates keep the dependency surface lower than a
  "all services" build.

Compile-checked shape:

```rust
op.write_with(path, bytes).if_match(etag).await?;
op.write_with(path, bytes).if_not_exists(true).await?;
```

Tradeoff:

- Conditional behavior is capability-driven. The backend must check
  capabilities at startup and fail loudly if `write_with_if_match` is disabled
  or unsupported for the configured service.

### `aws-sdk-s3`

`aws-sdk-s3` is the safest compatibility fallback.

Why:

- It exposes `if_match` and `if_none_match` directly on `PutObject`.
- Provider documentation often includes AWS SDK examples.
- It is the best escape hatch for exact AWS/R2/Yandex/B2 behavior debugging.

Compile-checked shape:

```rust
client
  .put_object()
  .bucket(bucket)
  .key(key)
  .if_match(etag)
  .body(ByteStream::from(bytes))
  .send()
  .await?;
```

Tradeoff:

- It has the largest dependency/build footprint and exposes a broad AWS model
  OmniFuse does not need for the first object backend.

### `s3`

`s3` is no longer a primary candidate for OmniFuse.

Reasons:

- `0.1.28` currently requires Rust `1.95`; the local `1.94.0-nightly` toolchain
  resolves `0.1.27`. OmniFuse declares `rust-version = "1.85"`.
- The normal high-level `PutObjectRequest` supports content headers, metadata,
  body, and checksums, but not `If-Match` or `If-None-Match`.
- Presign builders expose custom headers, but that does not solve normal backend
  upload paths.

This crate can be revisited if it adds first-class conditional write setters and
 supports OmniFuse's MSRV.

## Dependency Footprint

Measured in temporary Cargo manifests under `/private/tmp/omnifuse-s3-spike`.
Counts are unique `cargo tree` packages for `aarch64-apple-darwin`, normal and
build edges, excluding dev edges. The root spike package is included in the
count.

| Candidate manifest | Cargo.lock packages | Target tree packages |
|---|---:|---:|
| `opendal` with `services-s3` | 164 | 122 |
| `object_store` with `aws` | 215 | 131 |
| `aws-sdk-s3` + `aws-config` | 208 | 179 |
| `s3` default features | 206 | 145 |

The numbers are close enough that API fit matters more than raw package count.
`aws-sdk-s3` is still materially heavier in target tree size.

## Provider Compatibility Notes

| Provider | Conditional write status |
|---|---|
| AWS S3 | Supports `If-Match` and `If-None-Match` on `PutObject`. |
| MinIO AIStor | Documents `If-Match` and `If-None-Match` for `PutObject`. |
| Cloudflare R2 | Documents conditional `PutObject` support in S3 extensions; object API table also confirms the basic operations needed by OmniFuse. |
| Yandex Object Storage | Documents `If-Match` and `If-None-Match` for `PutObject`. |
| Backblaze B2 | Official S3-compatible API docs list `Put Object`, but do not explicitly confirm conditional `PutObject`; treat as unverified. |

## Recommendation

Use `object_store` for the first S3-compatible backend spike.

Implementation shape:

1. Extract reusable text merge decision logic from `omnifuse-wiki` into a shared
   module, then update the wiki backend to consume that shared API.
2. Add a new `omnifuse-s3` crate with a narrow internal
   adapter around `object_store::ObjectStore`.
3. Store per-object base metadata locally: ETag, optional version, size, and
   last-modified timestamp.
4. For new files, use `PutMode::Create`.
5. For modified files, use `PutMode::Update(UpdateVersion { e_tag, version })`.
6. On `Error::Precondition`, fetch remote content, run the shared text merge
   decision logic, and retry with the latest remote version.
7. Keep `aws-sdk-s3` as an adapter fallback only if a real provider fails the
   `object_store` path.

First implementation slice:

| Slice | Scope | Validation |
|---|---|---|
| Shared text merge | Move `MergeDecision`, `MergeResult`, `three_way_merge`, and `decide_merge` out of `omnifuse-wiki`. | Existing wiki merge tests still pass; no S3 dependency yet. |
| S3 adapter skeleton | Build config, object path mapping, metadata store, and `object_store` client construction behind a narrow adapter. | Unit tests with memory/local object store where possible. |
| Conditional sync loop | Implement create/update CAS paths and precondition-to-conflict mapping. | Local MinIO/RustFS live test matrix. |
| App/CLI wiring | Add mount args and cache identity after backend behavior is proven. | CLI smoke test with local S3-compatible endpoint. |

## Remaining Spike

Before wiring the backend into CLI/GUI, run a small live matrix:

| Environment | Required checks |
|---|---|
| Local MinIO/RustFS | list, head, get, put, delete, `PutMode::Create`, `PutMode::Update`, ETag persistence |
| Cloudflare R2 | same checks, especially conditional upload response mapping |
| Yandex Object Storage | same checks, verify ETag quoting and precondition status mapping |
| Backblaze B2 | same checks; decide whether unsupported conditional writes should disable merge-safe text sync |

No repository dependency was added in this spike.

## References

- [`object_store` crate](https://docs.rs/crate/object_store/0.13.2)
- [`object_store::PutMode`](https://docs.rs/object_store/0.13.2/object_store/enum.PutMode.html)
- [`OpenDAL` crate](https://docs.rs/crate/opendal/0.56.0)
- [`aws-sdk-s3` `PutObjectFluentBuilder`](https://docs.rs/aws-sdk-s3/latest/aws_sdk_s3/operation/put_object/builders/struct.PutObjectFluentBuilder.html)
- [`s3` crate source metadata](https://docs.rs/crate/s3/0.1.28/source/Cargo.toml.orig)
- [Cloudflare R2 S3 extensions](https://developers.cloudflare.com/r2/api/s3/extensions/)
- [Cloudflare R2 S3 API compatibility](https://developers.cloudflare.com/r2/api/s3/api/)
