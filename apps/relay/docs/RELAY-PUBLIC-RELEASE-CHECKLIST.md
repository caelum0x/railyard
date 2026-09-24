# Relay public release checklist

This is the manual, step-by-step path for publishing Relay as its own public
repository (planned: `caelum0x/relay`). It does not depend on GitHub Actions.
Every step runs on a workstation. The owner creates the repository. Nothing in
this checklist creates it.

Work through the sections in order. Check an item only after doing it.

## 1. Decide the release

- [ ] Pick the first Relay version. Relay keeps TensorZero's CalVer scheme
      (`YYYY.MM.PATCH`), so the first release is the month it ships, for
      example `2026.10.0`. It must be newer than the `2026.6.0` baseline.
- [ ] Set the version in `crates/Cargo.toml` (`[workspace.package] version`),
      then `cd crates && cargo check -p gateway` to refresh `Cargo.lock`.
- [ ] Set the same version in `clients/relay-js/package.json`,
      `clients/relay-python` and `clients/relay-ruby` if they are published at
      the same time.
- [ ] In `CHANGELOG.md`, rename `## Unreleased` to `## <version> (<date>)` and
      remove the "crate version still reads 2026.6.0" paragraph.

## 2. Extract the repository with history

Relay lives in `apps/relay` inside the Railyard monorepo. Publish only that
folder, with its history:

```bash
cd /path/to/railyard
git switch main && git pull
git subtree split --prefix=apps/relay -b relay-public
# or, in a scratch clone: git filter-repo --subdirectory-filter apps/relay
```

- [ ] Read `git log relay-public` from start to finish. Commit messages from
      the monorepo mention other Railyard products (Mint, Rago, Menivor,
      AutoGTM). Decide whether that is acceptable. If not, squash the history
      into one "Relay <version>" commit on top of an attribution commit that
      says where the code came from.
- [ ] Confirm the first commit on the branch still contains the unmodified
      TensorZero 2026.6.0 sources, so upstream authorship stays visible.

## 3. Scan for secrets

Run the scan on the extracted branch, including all of its history, not just
the working tree.

- [ ] `gitleaks detect --source . --log-opts="relay-public"` (or
      `trufflehog git file://. --branch relay-public`) reports nothing.
- [ ] `grep -rEn "(sk-[A-Za-z0-9]{20,}|sk-ant-|AKIA[0-9A-Z]{16}|npg_[A-Za-z0-9]+|postgres://[^ ]*:[^@ ]+@)" .`
      finds only placeholders and the demo password `relay-local-only` in
      `examples/relay/budgets`.
- [ ] No `.env`, `relay.env`, `*.pem` or `gcp_jwt_key.json` files are tracked.
- [ ] Any secret found in history is rotated at the provider first, then
      removed with `git filter-repo --replace-text`. Removing it from history
      does not make an exposed secret safe again.

## 4. License and attribution

- [ ] `LICENSE` is the unmodified Apache License 2.0 text.
- [ ] `NOTICE` names TensorZero as the upstream project, states the 2026.6.0
      baseline, says Relay is not affiliated with or endorsed by TensorZero,
      and lists what Relay added. Apache-2.0 section 4(d) requires shipping
      this file with every distribution, including the Docker image.
- [ ] Upstream files keep their original copyright and license headers.
      `git diff <baseline>..relay-public` shows no removed notices.
- [ ] Modified upstream files are identifiable as changed (the git history
      satisfies Apache-2.0 section 4(b); keep the history public, or add a
      "Modified by Relay" line to each changed file if history is squashed).
- [ ] The TensorZero name is used only to identify the upstream project. The
      TensorZero logo is not used as Relay branding. The README's upstream
      section is clearly labelled as describing TensorZero.
- [ ] `cd crates && cargo deny check licenses` passes with `crates/deny.toml`.
- [ ] UI and client dependencies contain no GPL or AGPL packages
      (`pnpm licenses list` in `ui/` and `clients/relay-js/`).

## 5. Replace upstream-only files

These files belong to TensorZero's project and would be wrong in Relay's
public repository:

- [ ] `SECURITY.md` tells people to email security@tensorzero.com. Replace it
      with Relay's own reporting address, or with GitHub private vulnerability
      reporting once the repository exists.
- [ ] `CLA.md` is TensorZero, Inc.'s contributor agreement. Remove it and use
      the Developer Certificate of Origin (`Signed-off-by`) or Relay's own
      agreement. Update `CONTRIBUTING.md` to match.
- [ ] `CITATION.cff` cites TensorZero. Either remove it or keep it only as the
      citation for the upstream project and add a Relay entry.
- [ ] `artifacthub-repo.yml` holds TensorZero's Artifact Hub repository id and
      owner email. Remove it.
- [ ] `RELEASE_GUIDE.md` describes TensorZero's release process. Replace it with
      a link to this checklist.
- [ ] `.github/workflows/`, `.github/dependabot.yml`, `.github/CODEOWNERS`,
      `.github/zizmor.yml` and `.buildkite/` are TensorZero's CI. They
      reference TensorZero's runners, secrets and Docker Hub account. Delete
      them. Relay's release process does not use GitHub Actions. Keep
      `.github/ISSUE_TEMPLATE` only after rewriting it for Relay.
- [ ] `.claude/`, `AGENTS.md`, `CLAUDE.md`, `.github/copilot-instructions.md`
      are contributor tooling notes. Keep or drop them on purpose.
- [ ] `OWNERSHIP.md` and `docs/RELAY-RELEASE-BOUNDARY.md` describe Railyard's
      internal boundary. Keep them only if that is useful to outside readers.

## 6. Fix links that only work inside the monorepo

- [ ] `README.md` links to `../vend/README.md`, and
      `docs/MIGRATING-FROM-TENSORZERO.md` to `../../vend/README.md`. Point them
      at Vend's public page or remove them.
- [ ] `README.md` "Relay product integration" runs
      `scripts/cloud-runtime-env.py` from the Railyard root. Replace it with
      plain environment-variable instructions.
- [ ] Mint, Rago and Railyard are mentioned as integrations. Each mention
      either links to something public or is described as optional.
- [ ] `grep -rn "\.\./\.\./" README.md docs/*.md examples/relay` finds nothing
      that leaves the repository.

## 7. Build and verify locally

Check free disk space first (`df -h`). A debug gateway build needs about 9 GB for
`target/`. Delete `crates/target` when done.

- [ ] `cd crates && cargo check -p gateway` passes.
- [ ] `cargo clippy -p gateway --all-targets -- -D warnings` passes.
- [ ] `cargo test` for the `relay-*` crates passes.
- [ ] Each shipped config validates:
      `gateway --validate-and-exit --config-file config/quickstart/tensorzero.toml`,
      and the same for `examples/relay/fallback-routing/tensorzero.toml`,
      `examples/relay/budgets/tensorzero.toml` and
      `config/tensorzero.toml` (with the variables it references set).
- [ ] `docker build -t relay-gateway:local .` succeeds on a clean checkout.
- [ ] The quickstart works from a clean clone with only `OPENAI_API_KEY` set:
      `docker compose -f docker-compose.quickstart.yml up --build`, then
      `examples/relay/openai-client/curl.sh` returns a completion.
- [ ] `examples/relay/price-catalog/quote.sh` returns the 900 micro-USD quote
      described in its README.
- [ ] The budgets walkthrough ends with `HTTP 429 relay_spend_cap_exceeded`.
- [ ] The fallback demo (invalid OpenAI key) answers from the `anthropic`
      variant.
- [ ] `python3 tools/relay_postgres_smoke.py` passes on a host that allows
      Postgres shared memory.

## 8. Publish the image

Pick one registry. The commands below use GitHub Container Registry and assume
you are logged in with `docker login ghcr.io`.

- [ ] Build both architectures and push:

      ```bash
      docker buildx create --name relay-builder --use --bootstrap
      docker buildx build --platform linux/amd64,linux/arm64 \
        --label org.opencontainers.image.source=https://github.com/caelum0x/relay \
        --label org.opencontainers.image.licenses=Apache-2.0 \
        --label org.opencontainers.image.revision=$(git rev-parse HEAD) \
        -t ghcr.io/caelum0x/relay-gateway:<version> \
        -t ghcr.io/caelum0x/relay-gateway:latest \
        --push .
      ```

- [ ] The image contains `LICENSE` and `NOTICE`:
      `docker run --rm --entrypoint ls ghcr.io/caelum0x/relay-gateway:<version> /usr/share/doc/relay`
      lists both files.
- [ ] `docker run --rm ghcr.io/caelum0x/relay-gateway:<version> --version`
      prints the new version.
- [ ] In `docker-compose.quickstart.yml`, set
      `image: ghcr.io/caelum0x/relay-gateway:<version>` and keep the `build:`
      block, so users can pull or build.
- [ ] Make the package public in the registry settings.

## 9. Create and push the repository (owner)

- [ ] Create `caelum0x/relay` as a public, empty repository (no README,
      license or .gitignore, to avoid a merge).
- [ ] `git push git@github.com:caelum0x/relay.git relay-public:main`
- [ ] Set the description ("Maintained fork of TensorZero: open-source LLM
      gateway with budgets, price catalog and fallback routing"), topics and
      website.
- [ ] Turn on private vulnerability reporting and point `SECURITY.md` at it.
- [ ] Protect `main` (no force pushes).
- [ ] Leave GitHub Actions disabled in the repository settings until Relay has
      its own reviewed workflows.

## 10. Tag and announce

- [ ] `git tag -s <version> -m "Relay <version>"` and `git push origin <version>`.
- [ ] Create the release from the web UI, or with
      `gh release create <version> --notes-file <changelog excerpt>`. Paste the
      changelog section. Link the image.
- [ ] State plainly in the release notes that Relay is a fork of TensorZero
      2026.6.0 and not affiliated with TensorZero.
- [ ] After release, sync fixes back: land changes in the Railyard monorepo
      first, then `git subtree split` again and push, so both stay identical.
