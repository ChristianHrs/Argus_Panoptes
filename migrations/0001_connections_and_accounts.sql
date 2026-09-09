CREATE TABLE bank_sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    provider TEXT NOT NULL,

    provider_session_id TEXT NOT NULL UNIQUE,

    aspsp_name TEXT NOT NULL,
    aspsp_country TEXT NOT NULL,

    status TEXT NOT NULL,

    valid_until TEXT,

    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);


CREATE TABLE bank_accounts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    session_id INTEGER NOT NULL,

    provider_account_uid TEXT NOT NULL UNIQUE,

    name TEXT,
    currency TEXT,
    cash_account_type TEXT,

    raw_json TEXT NOT NULL,

    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,

    FOREIGN KEY (session_id)
        REFERENCES bank_sessions(id)
        ON DELETE CASCADE
);
