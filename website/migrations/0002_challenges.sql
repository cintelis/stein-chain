-- Opening the inbox from a browser: a challenge the caller must answer with
-- the inbox's secret key (src/proof.js). Each is answered once and expires
-- after two minutes.
CREATE TABLE challenges (
  id      TEXT PRIMARY KEY,
  proof   TEXT NOT NULL,
  expires INTEGER NOT NULL
);
