-- Storage format 1. Initialization executes this schema in one transaction.
-- The caller configures the connection, owns BEGIN/COMMIT, inserts store metadata
-- and initial maintenance rows, and validates foreign keys before publication.
-- Do not add IF NOT EXISTS: an unexpected existing schema must fail validation.

CREATE TABLE store_metadata (
    singleton_id           INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    store_id               BLOB NOT NULL CHECK (length(store_id) = 16),
    owner_account_id       TEXT NOT NULL CHECK (
                               length(CAST(owner_account_id AS BLOB)) = 12
                               AND length(owner_account_id) = 12
                               AND owner_account_id NOT GLOB '*[^0-9]*'),
    canonical_owner_id     TEXT NOT NULL CHECK (
                               length(CAST(canonical_owner_id AS BLOB)) = 64
                               AND length(canonical_owner_id) = 64
                               AND canonical_owner_id NOT GLOB '*[^0-9a-f]*'),
    format_version         INTEGER NOT NULL CHECK (format_version >= 1),
    region                 TEXT NOT NULL CHECK (length(region) > 0),
    shard_count            INTEGER NOT NULL CHECK (shard_count = 256),
    shard_algorithm        TEXT NOT NULL CHECK (shard_algorithm = 'first_byte'),
    continuation_token_key BLOB NOT NULL CHECK (length(continuation_token_key) = 32),
    created_at_ms          INTEGER NOT NULL
) STRICT;

CREATE TABLE principals (
    principal_id      BLOB PRIMARY KEY NOT NULL CHECK (length(principal_id) = 16),
    name              TEXT NOT NULL COLLATE BINARY CHECK (
                          length(CAST(name AS BLOB)) BETWEEN 1 AND 128),
    policy_generation BLOB CHECK (length(policy_generation) = 16),
    is_enabled        INTEGER NOT NULL CHECK (is_enabled IN (0, 1)),
    policy_json       TEXT CHECK (length(CAST(policy_json AS BLOB)) <= 20480),
    created_at_ms     INTEGER NOT NULL,
    UNIQUE (name),
    CHECK ((policy_generation IS NULL AND policy_json IS NULL)
        OR (policy_generation IS NOT NULL AND policy_json IS NOT NULL))
) STRICT;

CREATE TABLE access_keys (
    access_key_id     TEXT PRIMARY KEY NOT NULL COLLATE BINARY CHECK (
                          length(CAST(access_key_id AS BLOB)) BETWEEN 1 AND 128),
    principal_id      BLOB NOT NULL,
    secret_access_key TEXT NOT NULL CHECK (
                          length(CAST(secret_access_key AS BLOB)) BETWEEN 1 AND 1024),
    created_at_ms     INTEGER NOT NULL,
    FOREIGN KEY (principal_id) REFERENCES principals (principal_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE INDEX access_keys_principal_idx ON access_keys (principal_id, access_key_id);

CREATE TABLE buckets (
    bucket_id               BLOB PRIMARY KEY NOT NULL CHECK (length(bucket_id) = 16),
    name                    TEXT COLLATE BINARY,
    lifecycle_generation    BLOB CHECK (length(lifecycle_generation) = 16),
    policy_generation       BLOB CHECK (length(policy_generation) = 16),
    block_public_acls       INTEGER CHECK (block_public_acls IN (0, 1)),
    ignore_public_acls      INTEGER CHECK (ignore_public_acls IN (0, 1)),
    block_public_policy     INTEGER CHECK (block_public_policy IN (0, 1)),
    restrict_public_buckets INTEGER CHECK (restrict_public_buckets IN (0, 1)),
    cors_rules_json         TEXT CHECK (length(CAST(cors_rules_json AS BLOB)) <= 262144),
    policy_json             TEXT CHECK (length(CAST(policy_json AS BLOB)) <= 20480),
    created_at_ms           INTEGER NOT NULL,
    CHECK ((policy_generation IS NULL AND policy_json IS NULL)
        OR (policy_generation IS NOT NULL AND policy_json IS NOT NULL)),
    CHECK ((block_public_acls IS NULL AND ignore_public_acls IS NULL
            AND block_public_policy IS NULL AND restrict_public_buckets IS NULL)
        OR (block_public_acls IS NOT NULL AND ignore_public_acls IS NOT NULL
            AND block_public_policy IS NOT NULL AND restrict_public_buckets IS NOT NULL))
) STRICT;

CREATE UNIQUE INDEX buckets_name_uq ON buckets (name) WHERE name IS NOT NULL;
CREATE INDEX buckets_retiring_idx ON buckets (bucket_id) WHERE name IS NULL;

CREATE TABLE bucket_lifecycle_rules (
    bucket_id                       BLOB NOT NULL,
    rule_index                      INTEGER NOT NULL CHECK (rule_index BETWEEN 0 AND 999),
    rule_id                         TEXT NOT NULL CHECK (length(rule_id) BETWEEN 1 AND 255),
    is_enabled                      INTEGER NOT NULL CHECK (is_enabled IN (0, 1)),
    filter_kind                     TEXT NOT NULL CHECK (filter_kind IN ('all', 'prefix', 'legacy_prefix')),
    filter_prefix                   BLOB NOT NULL CHECK (length(filter_prefix) <= 1024),
    expiration_days                 INTEGER CHECK (expiration_days BETWEEN 1 AND 2147483647),
    abort_incomplete_multipart_days INTEGER CHECK (abort_incomplete_multipart_days BETWEEN 1 AND 2147483647),
    PRIMARY KEY (bucket_id, rule_index),
    FOREIGN KEY (bucket_id) REFERENCES buckets (bucket_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    CHECK (expiration_days IS NOT NULL OR abort_incomplete_multipart_days IS NOT NULL),
    CHECK (filter_kind != 'all' OR length(filter_prefix) = 0)
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX bucket_lifecycle_rules_rule_id_uq
    ON bucket_lifecycle_rules (bucket_id, rule_id);

CREATE INDEX bucket_lifecycle_rules_expiration_idx ON bucket_lifecycle_rules (bucket_id)
    WHERE is_enabled = 1 AND expiration_days IS NOT NULL;

CREATE INDEX bucket_lifecycle_rules_abort_idx ON bucket_lifecycle_rules (bucket_id)
    WHERE is_enabled = 1 AND abort_incomplete_multipart_days IS NOT NULL;

CREATE TABLE blobs (
    row_id             INTEGER PRIMARY KEY AUTOINCREMENT CHECK (row_id > 0),
    blob_id            BLOB NOT NULL CHECK (length(blob_id) = 16),
    shard              INTEGER NOT NULL CHECK (shard BETWEEN 0 AND 255),
    state              TEXT NOT NULL CHECK (state IN ('reserved', 'ready', 'deleting')),
    write_kind         TEXT NOT NULL CHECK (write_kind IN (
                             'put_object', 'copy_object', 'upload_part',
                             'complete_multipart_upload')),
    target_bucket_id   BLOB CHECK (length(target_bucket_id) = 16),
    target_key         BLOB CHECK (length(target_key) BETWEEN 1 AND 1024),
    target_upload_id   TEXT CHECK (length(CAST(target_upload_id AS BLOB)) = 51),
    target_part_number INTEGER CHECK (target_part_number > 0),
    byte_length        INTEGER CHECK (byte_length >= 0),
    part_count         INTEGER CHECK (part_count > 0),
    etag               TEXT,
    md5_digest         BLOB CHECK (length(md5_digest) = 16),
    checksum_type      TEXT CHECK (checksum_type IN ('FULL_OBJECT', 'COMPOSITE')),
    checksums_json     TEXT CHECK (length(CAST(checksums_json AS BLOB)) <= 4096),
    created_at_ms      INTEGER NOT NULL,
    gc_pending         INTEGER NOT NULL CHECK (gc_pending IN (0, 1)),
    gc_retry_at_ms     INTEGER,
    gc_attempt_count   INTEGER NOT NULL CHECK (gc_attempt_count >= 0),
    gc_error_code      TEXT CHECK (length(gc_error_code) <= 64),
    UNIQUE (blob_id),
    CHECK (state <> 'reserved' OR (
        target_bucket_id IS NOT NULL AND target_key IS NOT NULL
        AND byte_length IS NULL AND part_count IS NULL AND etag IS NULL AND md5_digest IS NULL
        AND checksum_type IS NULL AND checksums_json IS NULL AND gc_pending = 1)),
    CHECK (state <> 'reserved' OR (
        (write_kind IN ('put_object', 'copy_object')
            AND target_upload_id IS NULL AND target_part_number IS NULL)
        OR (write_kind = 'upload_part'
            AND target_upload_id IS NOT NULL AND target_part_number IS NOT NULL)
        OR (write_kind = 'complete_multipart_upload'
            AND target_upload_id IS NOT NULL AND target_part_number IS NULL))),
    CHECK (state <> 'ready' OR (
        target_bucket_id IS NULL AND target_key IS NULL
        AND target_upload_id IS NULL AND target_part_number IS NULL
        AND byte_length IS NOT NULL AND etag IS NOT NULL
        AND md5_digest IS NOT NULL AND checksums_json IS NOT NULL)),
    CHECK (state <> 'ready' OR (
        (write_kind = 'complete_multipart_upload' AND part_count IS NOT NULL)
        OR (write_kind <> 'complete_multipart_upload' AND part_count IS NULL))),
    CHECK (state <> 'deleting' OR gc_pending = 1),
    CHECK (gc_pending = 1 OR (
        gc_retry_at_ms IS NULL AND gc_attempt_count = 0 AND gc_error_code IS NULL))
) STRICT;

CREATE INDEX blobs_gc_idx ON blobs (shard, row_id) WHERE gc_pending = 1;

CREATE TABLE blob_parts (
    blob_id        BLOB NOT NULL,
    part_number    INTEGER NOT NULL CHECK (part_number > 0),
    part_index     INTEGER NOT NULL CHECK (part_index BETWEEN 1 AND 10000),
    byte_offset    INTEGER NOT NULL CHECK (byte_offset >= 0),
    byte_length    INTEGER NOT NULL CHECK (byte_length >= 0),
    checksums_json TEXT NOT NULL CHECK (length(CAST(checksums_json AS BLOB)) <= 4096),
    PRIMARY KEY (blob_id, part_number),
    FOREIGN KEY (blob_id) REFERENCES blobs (blob_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX blob_parts_order_uq ON blob_parts (blob_id, part_index);

CREATE TABLE objects (
    row_id                 INTEGER PRIMARY KEY AUTOINCREMENT CHECK (row_id > 0),
    bucket_id              BLOB NOT NULL,
    key                    BLOB NOT NULL CHECK (length(key) BETWEEN 1 AND 1024),
    generation             BLOB NOT NULL CHECK (length(generation) = 16),
    blob_id                BLOB NOT NULL,
    content_type           TEXT,
    content_encoding       TEXT,
    content_disposition    TEXT,
    content_language       TEXT,
    cache_control          TEXT,
    expires_at_ms          INTEGER,
    user_metadata_json     TEXT NOT NULL CHECK (length(CAST(user_metadata_json AS BLOB)) <= 65536),
    tags_json              TEXT NOT NULL CHECK (length(CAST(tags_json AS BLOB)) <= 32768),
    last_modified_at_ms    INTEGER NOT NULL,
    lifecycle_anchor_at_ms INTEGER NOT NULL,
    UNIQUE (bucket_id, key),
    FOREIGN KEY (bucket_id) REFERENCES buckets (bucket_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY (blob_id) REFERENCES blobs (blob_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
) STRICT;

CREATE UNIQUE INDEX objects_blob_id_uq ON objects (blob_id);

CREATE TABLE multipart_uploads (
    row_id                  INTEGER PRIMARY KEY AUTOINCREMENT CHECK (row_id > 0),
    upload_id               TEXT NOT NULL COLLATE BINARY CHECK (length(CAST(upload_id AS BLOB)) = 51),
    bucket_id               BLOB NOT NULL,
    key                     BLOB NOT NULL CHECK (length(key) BETWEEN 1 AND 1024),
    initiator_id            TEXT NOT NULL CHECK (
                                length(CAST(initiator_id AS BLOB)) BETWEEN 1 AND 256),
    state                   TEXT NOT NULL CHECK (state IN ('open', 'completing')),
    completion_claim_id     BLOB CHECK (length(completion_claim_id) = 16),
    completion_blob_id      BLOB,
    completion_parts_digest BLOB CHECK (length(completion_parts_digest) = 32),
    checksum_algorithm      TEXT,
    checksum_type           TEXT CHECK (checksum_type IN ('FULL_OBJECT', 'COMPOSITE')),
    content_type            TEXT,
    content_encoding        TEXT,
    content_disposition     TEXT,
    content_language        TEXT,
    cache_control           TEXT,
    expires_at_ms           INTEGER,
    user_metadata_json      TEXT NOT NULL CHECK (length(CAST(user_metadata_json AS BLOB)) <= 65536),
    tags_json               TEXT NOT NULL CHECK (length(CAST(tags_json AS BLOB)) <= 32768),
    initiated_at_ms         INTEGER NOT NULL,
    UNIQUE (upload_id),
    FOREIGN KEY (bucket_id) REFERENCES buckets (bucket_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY (completion_blob_id) REFERENCES blobs (blob_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    CHECK ((state = 'open' AND completion_claim_id IS NULL
            AND completion_blob_id IS NULL AND completion_parts_digest IS NULL)
        OR (state = 'completing' AND completion_claim_id IS NOT NULL
            AND completion_blob_id IS NOT NULL AND completion_parts_digest IS NOT NULL))
) STRICT;

CREATE INDEX multipart_uploads_listing_idx ON multipart_uploads (bucket_id, key, upload_id);
CREATE UNIQUE INDEX multipart_uploads_completion_blob_id_uq
    ON multipart_uploads (completion_blob_id) WHERE completion_blob_id IS NOT NULL;

CREATE TABLE multipart_parts (
    upload_id           TEXT NOT NULL COLLATE BINARY CHECK (length(CAST(upload_id AS BLOB)) = 51),
    part_number         INTEGER NOT NULL CHECK (part_number > 0),
    generation          BLOB NOT NULL CHECK (length(generation) = 16),
    blob_id             BLOB NOT NULL,
    last_modified_at_ms INTEGER NOT NULL,
    PRIMARY KEY (upload_id, part_number),
    FOREIGN KEY (upload_id) REFERENCES multipart_uploads (upload_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY (blob_id) REFERENCES blobs (blob_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX multipart_parts_blob_id_uq ON multipart_parts (blob_id);

CREATE TABLE multipart_receipts (
    row_id                  INTEGER PRIMARY KEY AUTOINCREMENT CHECK (row_id > 0),
    upload_id               TEXT NOT NULL COLLATE BINARY CHECK (length(CAST(upload_id AS BLOB)) = 51),
    bucket_id               BLOB NOT NULL CHECK (length(bucket_id) = 16),
    key                     BLOB NOT NULL CHECK (length(key) BETWEEN 1 AND 1024),
    outcome                 TEXT NOT NULL CHECK (outcome IN ('completed', 'aborted')),
    completion_claim_id     BLOB CHECK (length(completion_claim_id) = 16),
    completion_blob_id      BLOB CHECK (length(completion_blob_id) = 16),
    completion_parts_digest BLOB CHECK (length(completion_parts_digest) = 32),
    result_json             TEXT CHECK (length(CAST(result_json AS BLOB)) <= 16384),
    finished_at_ms          INTEGER NOT NULL,
    UNIQUE (upload_id),
    CHECK ((completion_claim_id IS NULL AND completion_blob_id IS NULL
            AND completion_parts_digest IS NULL)
        OR (completion_claim_id IS NOT NULL AND completion_blob_id IS NOT NULL
            AND completion_parts_digest IS NOT NULL)),
    CHECK ((outcome = 'completed' AND completion_blob_id IS NOT NULL AND result_json IS NOT NULL)
        OR (outcome = 'aborted' AND result_json IS NULL))
) STRICT;

CREATE TABLE maintenance_sweeps (
    task          TEXT NOT NULL CHECK (task IN (
                       'gc_blobs', 'expire_objects', 'abort_uploads', 'purge_receipts')),
    shard         INTEGER NOT NULL,
    generation    BLOB NOT NULL CHECK (length(generation) = 16),
    cursor_row_id INTEGER NOT NULL CHECK (cursor_row_id >= 0),
    upper_row_id  INTEGER NOT NULL CHECK (upper_row_id >= cursor_row_id),
    started_at_ms INTEGER NOT NULL,
    PRIMARY KEY (task, shard),
    CHECK ((task = 'gc_blobs' AND shard BETWEEN 0 AND 255)
        OR (task <> 'gc_blobs' AND shard = -1))
) STRICT, WITHOUT ROWID;
