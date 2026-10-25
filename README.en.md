# Naxus

[中文](README.md)

A Rust artifact repository for local development and remote teams. **Publish new contents under the same crate name and version** to a mutable repository. Each change creates an immutable revision and snapshot. `cargo naxus` refreshes the selected version's revision; `--locked` retains the recorded contents.

Business projects configure one Naxus endpoint. Naxus proxies crates.io and explicitly configured public sparse registries, verifies original archive checksums, persists them, and serves subsequent requests from its cache. Admin, registry, health and metrics share one port. The same long-running service works locally, on a server, or in a container.

## Same-version publication and runtime architecture

Mutable repositories accept new contents under an existing coordinate; immutable repositories retain bytes and dependency semantics. Both retain immutable historical snapshots. Default client commands discover a new revision of the selected SemVer, while locked commands retain the recorded revision. Explicit update allows Cargo to choose another version within the manifest constraints.

```text
Business workspace -> Cargo client + credential provider -> Naxus single listener
                                                            |-- hosted snapshots
                                                            |-- ordered public mirrors and cache
                                                            |-- Admin, health and metrics
                                                                      |
                                                           SQLite metadata + original artifacts
```

Publication validates and synchronizes artifact files before atomically committing revisions, snapshots, idempotency records and HEAD. Consumers bind source identities and a snapshot, obtain a retention lease, run native Cargo, and commit Cargo.lock/naxus.lock through a durable journal. Same-version refresh and historical access do not depend on deleting Cargo caches.

Each proxy retains one logical registry identity. Verified archives are persisted after the first fetch; mirrors are tried in order and independent registries are kept separate. One writer owns a data root. The server never runs uploaded code. See [architecture](docs/architecture.md) for persistence order and boundaries.

## Build and start

Rust/Cargo **1.97 or newer** is required. Server and client currently support local Linux/macOS filesystems with file locking, atomic rename and directory synchronization. Windows is not supported. SQLite is bundled; no separate database, Redis or discovery service is required.

```sh
cargo build --workspace --release --locked
cargo install --path . --locked
cargo install --path crates/cargo-naxus --locked
cargo install --path crates/cargo-credential-naxus --locked
mkdir -p "$HOME/.local/state/naxus/logs"
export NAXUS_LOG_PATH="$HOME/.local/state/naxus/logs"
naxus serve --config zcf/application.yml
```

Keep the two client executables together on PATH. Defaults: `127.0.0.1:7443`, data `~/.naxus/repository`, mutable repository `team-dev`, crates.io proxy enabled. Unauthenticated mode requires both listening and advertised addresses to be loopback.

Open `http://127.0.0.1:7443/admin/`. The first administrator password is generated into `<data-root>/bootstrap/admin.password` with restricted permissions. First login requires a password change. Restarting preserves accounts. Create separate read/publish tokens for developers and CI.

Logging uses `${NAXUS_LOG_PATH:/usr/local/logs/${application.name}}`; local users should set `NAXUS_LOG_PATH` to a writable directory. See [operations](docs/operations.md), [configuration](docs/configuration.md), and [Docker](docker/README.md).

The [first-use guide](docs/quickstart.md) walks through login, tokens, a producer and a consumer. Companion clients run on developer machines or CI; the server image does not contain Cargo or a Rust toolchain.

## Use from a business workspace

Supply an authorized token through `NAXUS_TOKEN` or `service.token_file`; a token file must be readable only by its owner. Keep secrets out of manifests, configuration contents, URLs and Git.

```sh
cargo naxus configure --endpoint https://registry.example.com --repository team-dev
cargo naxus publish --workspace --atomic
cargo naxus publish -p company-sdk
```

Native Cargo packages a controlled source copy. Original manifests and `Cargo.lock` remain unchanged. The batch commits atomically against its expected HEAD. Partial publication preserves the private identity of unselected workspace dependencies, which must already exist in the repository. `--no-verify` skips Cargo's compilation check, while server archive and dependency checks remain mandatory.

Before publication, fixed archives and the original batch intent are persisted. The next online command recovers those bytes and the same idempotency key after interruption. A recovered publish does not also publish later source edits; run publish again for them. `cargo naxus publish --abort-pending` handles an unfinished batch without deleting committed packages. Ignore `.naxus/publication*` and `.naxus/publications/` in business repositories, and retain recovery data until the outcome is known. Pending publication blocks offline builds.

Consumer manifest:

```toml
[dependencies]
company-sdk = { version = "1.8", registry = "naxus" }
serde = { version = "1", features = ["derive"] }
```

```sh
cargo naxus build
cargo naxus run
cargo naxus check
cargo naxus test
cargo naxus update
cargo naxus build --locked
cargo naxus build --locked --offline
```

Default commands preserve the selected SemVer while checking revision changes. Manifest/resolution input changes and explicit `update` allow Cargo to select versions within declared constraints. Offline mode requires both locks and a populated local Cargo cache. Online locked commands still validate service identity, retained history and public materialization revocations.

Commit `Cargo.lock`, `naxus.lock`, and the secret-free `.naxus/config.toml`. Ignore `.naxus/client.lock` and `.naxus/transactions/`. A durable journal recovers the lock pair and refuses to overwrite changes outside the recorded transaction.

Normal `cargo build` follows Cargo's immutable registry/cache contract and **does not automatically refresh same-version contents**. Use `cargo naxus` for mutable development. Immutable releases also support native Cargo publish, download and yank APIs. [Cargo registry contract](https://doc.rust-lang.org/cargo/reference/registry-index.html)

## Sources and management

Omitting `naxus.proxies` enables crates.io. An explicit list replaces defaults; `proxies: []` disables public proxies. Mirrors of one logical source are tried in configured order. Independent registries retain separate UUIDs and source identities, including different contents under identical names and versions. A successful mirror missing a required version triggers bounded full-graph retries in new fixed sessions; index rows from different mirrors are never merged.

Indexes use TTL and conditional revalidation; verified archives are persisted by checksum. Changed checksums for known public coordinates are rejected. Upstream requests enforce origin, DNS and redirect restrictions and never forward Naxus credentials. Supported upstreams are public, unauthenticated HTTP sparse registries. Git index proxying, private upstream credentials and custom CA configuration are not provided.

Admin manages user roles, repository grants, tokens, paginated audit and revision history, snapshot retention pins, yank/unyank, rollback, logical removal, public cache clearing and durable physical deletion. Browser sessions are separate from Cargo tokens; writes validate Origin and CSRF. Admin mutations recheck the authenticated account generation, role and repository grants inside their database transaction. The UI has no external CDN dependency.

Publish tokens allow initial publication by default. Changing an already known coordinate requires explicit overwrite permission; exact package-name restrictions are also available. The HEAD transaction rechecks current account and token authorization. Password, enabled-state and grant changes invalidate old sessions and tokens. At least one enabled administrator must remain.

- Logical removal retains historical contents.
- Physical hosted deletion explicitly revokes selected revisions. HEAD, retained references, active build leases and readers block deletion. Purged historical access returns `410 Gone`.
- Clearing public cache does not ban a package or version. The same version and future upstream versions can be fetched again. Invalidated materializations require a normal unlocked command to materialize again.
- Shared live references retain object bytes. Generation fencing prevents old tasks deleting reinstalled objects. Failed tasks retain progress, use backoff and expose safe error categories and a retry action.

There is no deletion CLI. Roles and repository UUIDs bound current authorization; read tokens cannot publish, and publish tokens cannot call physical deletion. Public proxies are team-shared sources, not private upstreams with separate per-user access controls.

An expired Admin session clears old identity and page state and returns to login. A 403 preserves the session; startup network or 503 failures offer reconnect. Invalid management input is 400, denied scope is 403, state or retention conflicts are 409, and unknown management paths are 404 without a Cargo challenge. Error request_id matches the response header. See [HTTP semantics](docs/protocol.md).

## Runtime boundaries

One active writer owns a data root. The root must be owned by the service account; Unix permissions are tightened to 0700, and a symlink at the root itself is rejected. SQLite WAL/FULL and object-before-reference persistence provide atomic visibility. Startup resumes durable work and validates retained objects; unexplained loss or corruption prevents readiness. See [architecture](docs/architecture.md).

Docker defaults to UID/GID `10001:10001`, `/var/lib/naxus` data, `/usr/local/logs/naxus` logs and managed authentication. Use local ext4/XFS/Btrfs/F2FS/overlayfs or APFS/HFS+. Docker Desktop requires Linux native named volumes rather than host-shared data mounts.

Defaults are 256 MiB per artifact and a 2 GiB free-space reserve. Low space rejects new writes while existing complete objects remain readable. Upstream outages do not revoke cached graphs. Failed explicit refresh leaves both workspace locks untouched. NASA owns the single listener, signals, `/livez`, `/readyz` and `/metrics`.

Objects that never acquired a hosted or public reference may be collected after a one-hour grace period, while active batches, sessions and readers remain protected. Published historical artifacts are excluded from this automatic cleanup. Managed builds reject Git dependencies, `[patch]`, `[replace]` and Cargo configuration includes before network resolution. Path dependencies must belong to the same workspace.

No multi-writer HA, zero-downtime upgrades, enterprise SSO, public signup, Cargo owner/search APIs or server execution of uploaded code is provided. Build scripts and procedural macros run on consumers; authenticated publication does not establish code trust.

## Documentation

- [First use](docs/quickstart.md), [client commands and locks](docs/client.md), [architecture](docs/architecture.md)
- [Configuration](docs/configuration.md), [HTTP interfaces](docs/protocol.md), [operations and backups](docs/operations.md)
- [Docker and volumes](docker/README.md), [GitHub and image delivery](docs/delivery.md), [troubleshooting and support](docs/troubleshooting.md)
- [Core](crates/naxus-core/README.md), [store](crates/naxus-store/README.md), [registry](crates/naxus-registry/README.md), [server](crates/naxus-server/README.md), [client library](crates/naxus-client/README.md), [Cargo command](crates/cargo-naxus/README.md), [credential provider](crates/cargo-credential-naxus/README.md)

Licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE). See [security](SECURITY.md) and [contributing](CONTRIBUTING.md).
