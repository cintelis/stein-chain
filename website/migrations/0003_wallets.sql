-- Registered wallets: accounts whose holders proved, with a signed statement
-- (src/statement.js), that they hold them. The public key and signature are
-- kept so the proof can be checked again by anyone given the row. Nothing
-- about the person: no address of theirs but this one.
CREATE TABLE wallets (
  address    TEXT PRIMARY KEY,
  public_key BLOB NOT NULL,
  signature  BLOB NOT NULL,
  day        TEXT NOT NULL
);
CREATE INDEX wallets_day ON wallets (day);
