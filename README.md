# Spending tracker (Rust + SQLite + Enable Banking)

Personal notes. Pulls my Lloyds and Revolut transactions into SQLite so I can
query them directly.

## Prerequisites

- Rust stable via rustup.
- A C toolchain — SQLx builds SQLite from source (`build-essential pkg-config`
  on Linux, `xcode-select --install` on macOS).

## Two applications

Sandbox and production are separate applications with separate app IDs and
private keys. Keep two `.env` files and two databases; mixing mock and real
transactions in one file makes every later query quietly wrong.

|              | Sandbox                          | Production                 |
| ------------ | -------------------------------- | -------------------------- |
| Bank         | Mock ASPSP (FR)                  | Lloyds, Revolut (GB)       |
| Redirect     | `http://localhost:3000/callback` | `https://htod.uk/callback` |
| Connect flow | automatic                        | `--manual`                 |
| Database     | `spending-sandbox.db`            | `spending.db`              |

Register at <https://enablebanking.com/sign-in/> (magic link, no password).
The RSA private key is only offered once — download it immediately.


## Configure

```ini
ENABLE_BANKING_APP_ID=<application uuid>
ENABLE_BANKING_PRIVATE_KEY=./keys/private.pem
REDIRECT_URL=https://htod.uk/callback
DATABASE_URL=sqlite://spending.db
COUNTRY=GB
BANK_NAME=Lloyds Bank
PSU_TYPE=personal
```

Sandbox swaps in the localhost redirect, `COUNTRY=FR`, `BANK_NAME=Mock ASPSP`,
and the sandbox database.

`.gitignore`:

```
.env
keys/
*.db
*.db-wal
*.db-shm
```

## Run

```bash
cargo run -- banks gb                          # list ASPSPs, with consent limits
cargo run -- connect "Lloyds Bank" GB --manual # authorise + backfill
cargo run -- accounts                          # what's stored, consent countdown
cargo run -- sync                              # incremental, safe to repeat
```

Resolve the exact bank name from `banks gb` before connecting. There's no
stable ASPSP identifier and names change on rebrand. The `consent` column is
that bank's maximum validity; `connect` always asks for the maximum.

### Manual connect (production)

`--manual` skips the local callback server, since production won't accept an
`http://localhost` redirect.

1. Run the command. It prints the authorisation URL.
2. Open it, authorise at the bank.
3. The browser lands on `https://htod.uk/callback?code=...&state=...`. The
   page may not load — doesn't matter.
4. Copy the **full URL** from the address bar, paste it at the prompt, enter.

Paste the whole URL rather than just the code, so the CSRF state gets
validated. A bare code works but prints a warning. The code is single-use and
expires quickly, so don't leave the paste sitting for minutes.

### Automatic connect (sandbox)

Without `--manual`, it starts a callback server on the port in `REDIRECT_URL`,
opens the browser, and captures the code itself. Works with Mock ASPSP.

### Scheduling

```
0 7,19 * * *  cd /path/to/project && ./target/release/bank_analytics sync >> sync.log 2>&1
```

Twice a day, no more. Background fetching (no PSU headers) is commonly capped
at four per day, after which `ASPSP_RATE_LIMIT_EXCEEDED` comes back and the
sync backs off six hours.

## Query

```bash
sqlite3 spending.db
```

```sql
SELECT booking_date, account, counterparty_name, amount, currency
FROM v_transactions
WHERE status = 'BOOK'
ORDER BY booking_date DESC
LIMIT 50;

SELECT month, currency, spent, received
FROM v_monthly_spend
ORDER BY month DESC;
```

Top counterparties, last 90 days:

```sql
SELECT counterparty_name,
       COUNT(*) AS n,
       -SUM(amount_minor) / 100.0 AS spent
FROM transactions
WHERE status = 'BOOK'
  AND amount_minor < 0
  AND booking_date >= date('now', '-90 days')
GROUP BY counterparty_name
ORDER BY spent DESC
LIMIT 20;
```

Categorise, and have it stick across re-syncs:

```sql
INSERT INTO categories (name) VALUES ('Groceries');

INSERT INTO transaction_categories (account_id, dedupe_key, category_id)
SELECT t.account_id, t.dedupe_key, c.id
FROM transactions t, categories c
WHERE c.name = 'Groceries'
  AND t.counterparty_name LIKE '%SAINSBURY%';
```

Labels key on `(account_id, dedupe_key)`, not row id, so they survive a
rebuild or re-import.

Anything not yet modelled is still in `raw_json`:

```sql
SELECT json_extract(raw_json, '$.merchant_category_code') FROM transactions LIMIT 5;
```

## Things that will bite me

**Backfill runs once, right after authorisation.** Full history is generally
only reachable for about an hour after consent is granted; after that most
ASPSPs restrict to the last 90 days. `connect` backfills immediately with
`strategy=longest`. If it fails, re-authorise — don't wait for the next sync.
Check `accounts` afterwards: the "since" date shows how much history actually
landed.

**Watch for `[TRUNCATED]` in sync output.** It means the page cap was hit with
more data pending. The date range printed on every sync is there so silent
truncation by the bank is visible.

**Account IDs are session-scoped.** Re-authorising gives new session and
account UIDs for the same accounts. Accounts key on `identification_hash`,
which is stable across sessions, so history stays attached through renewals.

**Pending transactions are replaced, not merged.** Every sync deletes and
re-inserts the `PDNG` rows. Filter on `status = 'BOOK'` for anything that
needs to be stable. Revolut will produce plenty of pending; Mock ASPSP
produces none, so this path is untested until Revolut is connected. A card
payment settling at a different amount than it authorised for looks like a
duplicate for one sync cycle — it isn't.

**Currencies don't sum.** `XXX` comes back for multi-currency accounts (expect
it from Revolut). Group by currency; never `SUM(amount_minor)` across the
whole table.

**Sessions expire early.** `EXPIRED_SESSION` can arrive well before
`valid_until`. The sync marks it locally; re-run `connect`. `accounts` shows
the countdown, but `last sync` is the number that actually tells me it's
working.

**JWT "can not be issued in the future" means clock drift**, not a code
problem. Happens after suspend/resume. `sudo hwclock -s` on WSL,
`sudo sntp -sS time.apple.com` on macOS. `iat` is backdated 60s, which covers
small skew but not a clock that's minutes out.

**Sandbox ≠ production.** Mock ASPSP ignores `date_from`, batches at 100, and
reports no pending. `banks gb` returns nothing in sandbox — expected, since
sandbox apps only get Mock ASPSP.

**Dedupe falls back to a synthetic key** when a bank omits `entry_reference`.
Mock ASPSP supplies it for every row, so the fallback is currently untested.
Run `SELECT entry_reference IS NULL, COUNT(*) FROM transactions GROUP BY 1`
after connecting each real bank; if anything comes back NULL, inspect the
payload and tighten the key before trusting the deduplication.

## Layout

```
migrations/0001_init.sql   schema, indexes, triggers, analytics views
src/main.rs                CLI: banks / connect / sync / accounts
src/enable_banking.rs      API client, pagination, error classification
src/sync.rs                backfill vs incremental, retry and backoff
src/store.rs               all SQL writes
src/callback.rs            local redirect server (sandbox only)
src/db.rs                  pool setup and migrations
```

Migrations run on startup. While the schema is still moving and holds nothing
but mock data, edit `0001` and delete the database — SQLx checksums applied
migrations and refuses to run an edited one. Once real transactions are in,
add `0002`.

## If manual paste gets tedious

htod.uk is on Cloudflare, so a named tunnel could map
`https://htod.uk/callback` to `localhost:3000` and the automatic flow would
work in production. Needs a separate `CALLBACK_PORT` env var, because
`callback_port()` would otherwise derive 443 from the https URL. Only worth it
if re-authorisation becomes frequent — at ~180 day consents, it won't.
