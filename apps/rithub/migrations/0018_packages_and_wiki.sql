-- Package registry (npm, generic files, OCI images) per repository.
--
-- Bytes live content-addressed under $RITHUB_DATA_ROOT/packages/<repo_id>/
-- sha256/<aa>/<hex>; these tables hold names, versions and pointers. Access
-- follows the repository: read to download, write to publish.

CREATE TABLE IF NOT EXISTS packages (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id    UUID        NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    kind       TEXT        NOT NULL CHECK (kind IN ('npm', 'generic', 'oci')),
    name       TEXT        NOT NULL,
    -- npm dist-tags ({"latest": "1.2.0"}).
    dist_tags  JSONB       NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, kind, name)
);

-- One stored file of one version (npm tarball, generic file). Immutable:
-- re-publishing a version is refused; delete first (generic only).
CREATE TABLE IF NOT EXISTS package_files (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    package_id UUID        NOT NULL REFERENCES packages(id) ON DELETE CASCADE,
    version    TEXT        NOT NULL,
    filename   TEXT        NOT NULL,
    sha256     TEXT        NOT NULL,
    size_bytes BIGINT      NOT NULL,
    -- npm: the version manifest; generic: {} or caller metadata.
    metadata   JSONB       NOT NULL DEFAULT '{}'::jsonb,
    created_by UUID        NULL REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (package_id, version, filename)
);
CREATE INDEX IF NOT EXISTS package_files_package_idx ON package_files (package_id);

-- OCI blobs known to a repository (layers, configs, manifests).
CREATE TABLE IF NOT EXISTS oci_blobs (
    repo_id    UUID        NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    digest     TEXT        NOT NULL,
    size_bytes BIGINT      NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repo_id, digest)
);

-- OCI manifest references: a tag or a digest pointing at a manifest blob.
CREATE TABLE IF NOT EXISTS oci_manifests (
    package_id UUID        NOT NULL REFERENCES packages(id) ON DELETE CASCADE,
    reference  TEXT        NOT NULL,
    digest     TEXT        NOT NULL,
    media_type TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (package_id, reference)
);
