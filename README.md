# Bank analytics starter (Rust + SQLite + GoCardless Bank Account Data)

## 1. Prerequisites

- Rust stable via rustup.
- A C toolchain (SQLx's bundled SQLite builds SQLite from source).
- GoCardless Bank Account Data user secrets.

Linux example:

```bash
sudo apt update
sudo apt install build-essential pkg-config
```

macOS:

```bash
xcode-select --install
```

## 2. Configure

```bash
cp .env.example .env
```

Fill in `GOCARDLESS_SECRET_ID` and `GOCARDLESS_SECRET_KEY`.

For initial development, `REDIRECT_URL=https://example.com` is enough for a manual flow: after bank authentication you can return to the terminal and run sync.

## 3. Run

List UK institutions:

```bash
cargo run -- banks
```

Create consent for one institution:

```bash
cargo run -- connect SANDBOXFINANCE_SFIN0000
```

Open the URL printed by the command and approve access. Then sync:

```bash
cargo run -- sync
```

The SQLite file is `spending.db` by default.

## 4. Try some analytics

```bash
sqlite3 spending.db
```

```sql
SELECT booking_date, description, amount_minor / 100.0 AS amount, currency
FROM transactions
ORDER BY booking_date DESC
LIMIT 50;

SELECT substr(booking_date, 1, 7) AS month,
       -SUM(amount_minor) / 100.0 AS spend
FROM transactions
WHERE amount_minor < 0 AND currency = 'GBP'
GROUP BY month
ORDER BY month;
```

## Notes

- The starter ingests only `booked` transactions. Pending transactions can change/disappear and are better handled separately.
- It stores the raw JSON next to normalized columns so you can enrich/re-normalize later.
- The fallback dedupe key should be strengthened after inspecting your own bank's actual transaction payload.
