-- Spoolbox schema v1 (docs/spec/03-architecture.md §9, ADR-003..ADR-009).
-- Times are Unix microseconds (UTC). `seq` orders messages and is stable.

CREATE TABLE messages (
    seq              INTEGER PRIMARY KEY,
    id               TEXT    NOT NULL UNIQUE,
    namespace_id     INTEGER NOT NULL DEFAULT 0,
    received_at      INTEGER NOT NULL,
    -- received: stored, not yet indexed | indexed | parse_failed
    status           TEXT    NOT NULL DEFAULT 'received',
    parse_error      TEXT,
    source           TEXT    NOT NULL DEFAULT 'smtp',
    size             INTEGER NOT NULL,
    raw_sha256       TEXT    NOT NULL,
    read             INTEGER NOT NULL DEFAULT 0,
    subject          TEXT    NOT NULL DEFAULT '',
    from_name        TEXT    NOT NULL DEFAULT '',
    from_addr        TEXT    NOT NULL DEFAULT '',
    message_id_hdr   TEXT,
    header_date      INTEGER,
    snippet          TEXT    NOT NULL DEFAULT '',
    attachment_count INTEGER NOT NULL DEFAULT 0,
    inline_count     INTEGER NOT NULL DEFAULT 0,
    problems         TEXT
) STRICT;

CREATE INDEX idx_messages_list ON messages (namespace_id, received_at DESC, seq DESC);
CREATE INDEX idx_messages_message_id ON messages (message_id_hdr);
CREATE INDEX idx_messages_unread ON messages (namespace_id) WHERE read = 0;

-- SMTP envelope: the protocol-level truth, never derived from headers (ADR-006).
CREATE TABLE envelopes (
    message_seq INTEGER PRIMARY KEY REFERENCES messages (seq) ON DELETE CASCADE,
    mail_from   TEXT NOT NULL,
    mail_params TEXT NOT NULL DEFAULT '[]',
    helo        TEXT NOT NULL DEFAULT '',
    remote_ip   TEXT NOT NULL DEFAULT '',
    tls         TEXT,
    auth_user   TEXT
) STRICT;

-- kind: envelope | from | to | cc | bcc | reply_to
CREATE TABLE recipients (
    message_seq INTEGER NOT NULL REFERENCES messages (seq) ON DELETE CASCADE,
    kind        TEXT    NOT NULL,
    ordinal     INTEGER NOT NULL,
    name        TEXT    NOT NULL DEFAULT '',
    addr        TEXT    NOT NULL,
    addr_lower  TEXT    NOT NULL,
    PRIMARY KEY (message_seq, kind, ordinal)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_recipients_addr ON recipients (addr_lower, kind);

-- Raw message, byte-exact as received (codec: zstd | none).
CREATE TABLE blobs (
    message_seq INTEGER PRIMARY KEY REFERENCES messages (seq) ON DELETE CASCADE,
    codec       TEXT NOT NULL,
    data        BLOB NOT NULL
) STRICT;

CREATE TABLE parts (
    message_seq       INTEGER NOT NULL REFERENCES messages (seq) ON DELETE CASCADE,
    part_id           TEXT    NOT NULL,
    parent            TEXT,
    content_type      TEXT    NOT NULL,
    charset           TEXT,
    disposition       TEXT,
    filename          TEXT,
    content_id        TEXT,
    content_language  TEXT,
    transfer_encoding TEXT,
    offset_header     INTEGER NOT NULL,
    offset_body       INTEGER NOT NULL,
    offset_end        INTEGER NOT NULL,
    decoded_size      INTEGER NOT NULL,
    is_attachment     INTEGER NOT NULL,
    is_inline         INTEGER NOT NULL,
    sha256            TEXT,
    md5               TEXT,
    sha1              TEXT,
    PRIMARY KEY (message_seq, part_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE tags (
    id        INTEGER PRIMARY KEY,
    name      TEXT NOT NULL,
    name_fold TEXT NOT NULL UNIQUE
) STRICT;

-- source: manual | x-tags | plus | username | rule
CREATE TABLE message_tags (
    message_seq INTEGER NOT NULL REFERENCES messages (seq) ON DELETE CASCADE,
    tag_id      INTEGER NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    source      TEXT    NOT NULL,
    PRIMARY KEY (message_seq, tag_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_message_tags_tag ON message_tags (tag_id);

-- Full-text index (ADR-007). rowid = messages.seq. Contentless: rows are deleted explicitly.
CREATE VIRTUAL TABLE message_fts USING fts5 (
    subject, addresses, body, filenames, headers,
    tokenize = 'trigram',
    content = '',
    contentless_delete = 1
);

-- Durable background work (ADR-009). state: pending | claimed | failed
CREATE TABLE outbox (
    id          INTEGER PRIMARY KEY,
    kind        TEXT    NOT NULL,
    message_seq INTEGER REFERENCES messages (seq) ON DELETE CASCADE,
    payload     TEXT    NOT NULL DEFAULT '{}',
    state       TEXT    NOT NULL DEFAULT 'pending',
    attempts    INTEGER NOT NULL DEFAULT 0,
    next_at     INTEGER NOT NULL,
    lease_until INTEGER,
    last_error  TEXT
) STRICT;

CREATE INDEX idx_outbox_due ON outbox (state, next_at);

-- Ordered change log for live updates and resumable event streams (ADR-009).
CREATE TABLE events (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    at         INTEGER NOT NULL,
    kind       TEXT    NOT NULL,
    message_id TEXT,
    data       TEXT    NOT NULL DEFAULT '{}'
) STRICT;

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;
