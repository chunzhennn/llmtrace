# Releases and container packages

Pushing a version tag runs `.github/workflows/release.yml`. The tag must match
`crates/llmtrace/Cargo.toml`, for example `v0.1.0` for crate version `0.1.0`.
The workflow can also be started manually with an existing version tag selected
as its ref; running it against a branch is rejected.

The release first runs the reusable Verify workflow: a full-history Gitleaks
scan, frontend checks and tests, Rust formatting and Clippy, functional/database
tests, and migration verification. A failed check prevents image publication.
The optional performance and live-provider tests are not part of a release.

## Published artifacts

Two native Linux runners build the existing Dockerfile for `amd64` and `arm64`.
The UI is built and embedded in each binary. Each image is smoke-tested with
`--version` (matching the release tag) and `--check-config`, and its exact binary
is extracted for the downloadable archive. Binary builds use Debian Bookworm, requiring glibc 2.36
or newer; they are not static musl binaries.

GitHub Release assets:

- `llmtrace-<version>-linux-amd64.tar.gz`
- `llmtrace-<version>-linux-arm64.tar.gz`
- `SHA256SUMS`

Each archive contains the binary, README, and the two example configurations.
Verify downloads with `sha256sum --check SHA256SUMS` after downloading both archives.
Configure a real database and admin credentials before starting the service.

The multi-architecture container is published to
`ghcr.io/chunzhennn/llmtrace:<version>`, with an additional `v<version>` tag.
Stable releases update `latest`; prereleases such as `v0.2.0-rc.1` do not.
`latest` tracks the most recently published stable version, so publish stable
versions in increasing order. OCI labels link the package to its repository,
commit and version; BuildKit includes provenance and an SBOM.

```bash
docker pull ghcr.io/chunzhennn/llmtrace:0.1.0
```

Runtime configuration, Postgres and persistent spool storage are supplied
separately, as described in the main README and the LiteLLM guide.

## Publishing a version

1. Update the crate version and `Cargo.lock` together, and commit the release
   changes. For the first release, the current version is already `0.1.0`.
2. Push the commits and an annotated tag:

   ```bash
   git push origin main
   git tag -a v0.1.0 -m "Release v0.1.0"
   git push origin v0.1.0
   ```

3. Wait for the Release workflow. Both architecture builds must succeed before
   the version manifest and release assets are published. A failed upload leaves
   a draft that a rerun can resume. An already published release is not replaced;
   use a new version tag for changed artifacts.

Authentication uses the workflow's `GITHUB_TOKEN`: image jobs have
`packages: write`, and only the final publishing job has `contents: write`.
No Docker Hub password, personal access token, or LLM provider key is required.
The repository or organization must permit Actions to publish packages. If an
existing GHCR package is not linked to this repository, grant it Actions access.
New packages may need their visibility changed to public in GitHub's package
settings for anonymous pulls.

These mechanics follow GitHub's [container publishing guide](https://docs.github.com/en/actions/tutorials/publish-packages/publish-docker-images)
and the [GitHub CLI release commands](https://cli.github.com/manual/gh_release_create).
Read the [credential audit](security-audit.md) for the initial history scan and
the precise placeholder exclusions used by CI.
