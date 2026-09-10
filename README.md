# Spending tracker (Rust + SQLite + Enable Banking)
 
Pulls transactions from Open Banking accounts into a local SQLite database you
can query directly.
 
## 1. Prerequisites
 
- Rust stable via rustup.
- A C toolchain — SQLx builds SQLite from source.
- An Enable Banking application (sandbox is free and needs no contract).
Linux:
 
```bash
sudo apt update
sudo apt install build-essential pkg-config
```
 
macOS:
 
```bash
xcode-select --install
```
 
## 2. Register an application
 
1. Sign in at <https://enablebanking.com/sign-in/> (magic link, no password).
2. Create an application. Pick **sandbox** to start.
3. Download the RSA private key when it's offered — it is shown once.
4. Set the redirect URL on the application to `http://localhost:3000/callback`.
   It must match `REDIRECT_URL` exactly, including the port.
5. For a sandbox app, add accounts and transactions under the **Mock ASPSP**
   tab in the control panel. That's the data you'll be syncing.
## 3. Configure
 
```bash
cp .env.example .env
```
 
```ini
ENABLE_BANKING_APP_ID=<application uuid from the control panel>
ENABLE_BANKING_PRIVATE_KEY=./keys/private.pem
REDIRECT_URL=http://localhost:3000/callback
DATABASE_URL=sqlite://spending.db
 
# Mock ASPSP is registered under FR, not GB
COUNTRY=FR
BANK_NAME=Mock ASPSP
PSU_TYPE=personal
```
 
Keep the key and `.env` out of version control:
 
```
.env
keys/
*.db
*.db-wal
*.db-shm
```
 
Use a separate database for sandbox work (`sqlite://spending-sandbox.db`).
Mixing mock and real transactions in one file makes every later query wrong in
a way that is easy to miss.
 
## 4. Run
 
Check the key and connectivity, and see what each bank allows:
 
```bash
cargo run -- banks fr
```
 
The `consent` column is that ASPSP's maximum consent validity; `connect`
requests the maximum rather than a fixed window.
 
Authorise:
 
```bash
cargo run -- connect "Mock ASPSP" FR
```
 
This starts a local callback server, opens the browser, waits for the
redirect, validates the CSRF state against the database, exchanges the code for
a session, then **immediately backfills history**. Don't interrupt it — see the
one-hour note below.
 
Afterwards:
 
```bash
cargo run -- accounts   # what's stored, and when it last synced
cargo run -- sync       # incremental pull, safe to run repeatedly
```
 
`sync` is idempotent. Schedule it once or twice a day:
 
```
0 7,19 * * *  cd /path/to/project && ./target/release/spending sync >> sync.log 2>&1
```
 
Don't schedule it more often. Most ASPSPs cap background fetching (no PSU
headers) at four per day and return `ASPSP_RATE_LIMIT_EXCEEDED` beyond that.
The sync backs off six hours when that happens.
 
## 5. Query
 
```bash
sqlite3 spending.db
```
 
Two views do the joins for you. `v_transactions` flattens account, bank and
category:
 
```sql
SELECT booking_date, account, counterparty_name, amount, currency
FROM v_transactions
WHERE status = 'BOOK'
ORDER BY booking_date DESC
LIMIT 50;
```
 
```sql
SELECT month, currency, spent, received
FROM v_monthly_spend
ORDER BY month DESC;
```
 
Top counterparties over the last 90 days:
 
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
 
Categorise something, and have it stick across re-syncs:
 
```sql
INSERT INTO categories (name) VALUES ('Groceries');
 
INSERT INTO transaction_categories (account_id, dedupe_key, category_id)
SELECT t.account_id, t.dedupe_key, c.id
FROM transactions t, categories c
WHERE c.name = 'Groceries'
  AND t.counterparty_name LIKE '%SAINSBURY%';
```
 
Labels are keyed on `(account_id, dedupe_key)` rather than row id, so they
survive a rebuild or re-import.
 
Anything not yet modelled is still there — every table keeps the original
payload in `raw_json`:
 
```sql
SELECT json_extract(raw_json, '$.merchant_category_code') FROM transactions LIMIT 5;
```
 
## 6. Going to production
 
A production application needs either a signed contract or **restricted
activation**: link your own accounts in the control panel, and only those
accounts are reachable. Link *every* account you intend to use — authorisation
will otherwise succeed and return an empty account list, which looks like a
bug in your code but isn't.
 
Then swap `ENABLE_BANKING_APP_ID`, `ENABLE_BANKING_PRIVATE_KEY`,
`DATABASE_URL`, and run `connect "Lloyds Bank" GB`. Resolve the exact name from
`cargo run -- banks gb` first; there is no stable ASPSP identifier and names
change on rebrand.
 
## Layout
 
```
migrations/0001_init.sql   schema, indexes, triggers, analytics views
src/main.rs                CLI: banks / connect / sync / accounts
src/enable_banking.rs      API client, pagination, error classification
src/sync.rs                backfill vs incremental, retry and backoff
src/store.rs               all SQL writes
src/callback.rs            local OAuth redirect server
src/db.rs                  pool setup and migrations
```
 
Migrations run automatically on startup. While the schema is still changing and
holds nothing but mock data, edit `0001` and delete the database file — SQLx
checksums applied migrations and will refuse to run an edited one. Once real
transactions are in there, add `0002` instead.
 