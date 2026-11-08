CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value ANY NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS sounds (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    duration_secs REAL NOT NULL CHECK(duration_secs > 0),
    byte_length INTEGER NOT NULL CHECK(byte_length > 0),
    position INTEGER NOT NULL,
    hotkey TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS sounds_position ON sounds(position);
CREATE TABLE IF NOT EXISTS sound_data (
    sound_id INTEGER NOT NULL REFERENCES sounds(id) ON DELETE CASCADE,
    chunk_index INTEGER NOT NULL CHECK(chunk_index >= 0),
    content BLOB NOT NULL CHECK(length(content) BETWEEN 1 AND 32768),
    PRIMARY KEY(sound_id, chunk_index)
) STRICT, WITHOUT ROWID;
