-- Phase E3: binary release assets.
--
-- A release (0005) names a tag over a commit; its *source* archive is produced
-- on demand from the git tree. Release *assets* are arbitrary uploaded binaries
-- (compiled artifacts, installers, checksums) attached to a release. The blob
-- bytes live on the durable data-root filesystem at `storage_path`; this table
-- holds only the metadata and the pointer.

CREATE TABLE IF NOT EXISTS release_assets (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    release_id   UUID        NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    filename     TEXT        NOT NULL,
    content_type TEXT        NOT NULL DEFAULT 'application/octet-stream',
    size_bytes   BIGINT      NOT NULL,
    -- Absolute path to the stored blob under $RITHUB_DATA_ROOT/release-assets/.
    storage_path TEXT        NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A filename is unique within a release (re-upload must delete first).
    UNIQUE (release_id, filename)
);

CREATE INDEX IF NOT EXISTS release_assets_release_idx ON release_assets (release_id);
