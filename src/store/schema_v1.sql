CREATE TABLE prs (
    account TEXT NOT NULL,
    tab TEXT NOT NULL,
    key TEXT NOT NULL,
    repo TEXT NOT NULL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    url TEXT NOT NULL,
    author TEXT NOT NULL,
    is_draft INTEGER NOT NULL,
    reasons TEXT NOT NULL,
    team TEXT,
    is_direct INTEGER NOT NULL,
    last_activity_at TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (account, tab, key)
);

CREATE TABLE activity (
    id TEXT PRIMARY KEY,
    pr_key TEXT NOT NULL,
    seen_at TEXT NOT NULL
);

CREATE INDEX activity_pr_key ON activity (pr_key);

CREATE TABLE alerts (
    pr_key TEXT PRIMARY KEY,
    state TEXT NOT NULL
);

CREATE TABLE commands (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    pr_key TEXT,
    until TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
