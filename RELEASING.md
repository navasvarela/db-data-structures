# Releasing

Releases are published to [crates.io](https://crates.io/crates/db-data-structures) by the
`Release` workflow whenever a `v*` tag is pushed. It authenticates with
[trusted publishing](https://crates.io/docs/trusted-publishing), so no crates.io API token is
stored in the repository.

## One-time setup

Trusted publishing can only be configured for a crate that already exists on crates.io, so the
very first version is published by hand.

1. Sign in to [crates.io](https://crates.io) with GitHub and verify your email address.
2. Create an API token under **Account Settings → API Tokens** with the `publish-new` scope.
3. Publish the first version from a clean checkout of `main`:

   ```bash
   cargo login
   ```

   ```bash
   cargo publish
   ```

4. Revoke the token, since the workflow will not need it.
5. On the crate's page, open **Settings → Trusted Publishing** and add a GitHub publisher with:
   - Repository owner: `navasvarela`
   - Repository name: `db-data-structures`
   - Workflow filename: `release.yml`
   - Environment: `crates-io`

## Publishing a version

1. Bump `version` in `Cargo.toml`, run `cargo test` so `Cargo.lock` picks it up, and merge the
   change to `main`.
2. Tag the merged commit with the same version and push the tag:

   ```bash
   git tag v0.2.0
   ```

   ```bash
   git push origin v0.2.0
   ```

The workflow refuses to publish if the tag does not match the version in `Cargo.toml`, and runs
the tests before publishing. docs.rs builds the documentation for each published version
automatically.
