-- The inbox: sealed messages and the UTC day each arrived. Nothing about the
-- sender is kept, and the envelope can be opened only with the inbox's
-- secret key (see website/README.md).
CREATE TABLE messages (
  id       INTEGER PRIMARY KEY AUTOINCREMENT,
  day      TEXT NOT NULL,
  envelope BLOB NOT NULL
);
CREATE INDEX messages_day ON messages (day);
