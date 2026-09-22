# Spending tracker (Rust + SQLite)

Personal notes. Imports downloaded bank statements from Lloyds, Revolut and
Amex into SQLite, categorises them, and lets me query the lot.

No API. Enable Banking doesn't cover the UK (EEA only, post-Brexit), and every
UK provider that does — Plaid, TrueLayer, Yapily — puts production access
behind business onboarding. Statements it is.

## Prerequisites

- Rust stable.
- A C toolchain — SQLx builds SQLite from source (`build-essential
  pkg-config` on Linux, `xcode-select --install` on macOS).
- `sqlite3` CLI for running the seed files.

`.env` is optional; `DATABASE_URL` defaults to `sqlite://spending.db`.

`.gitignore`:

```
*.db
*.db-wal
*.db-shm
bank_statements/
seeds/
```

Both `bank_statements/` and `seeds/` are ignored: one holds statements, the
other holds rules containing payee names.

## Getting the statements

```
bank_statements/
  lloyds/    transactions_2025_03_01-2026_03_01.csv
  revolut/   consolidated_statement_2023_10_02_2026_09_16.csv
  amex/      2026_transactions.csv
```

**Lloyds** — desktop internet banking only, not the app. Open the account,
scroll to the bottom of the transaction list, click Export, pick a date range.
Roughly 12 months of history available. The quoted 150-row cap didn't apply:
one export came back with 717 rows.

**Revolut** — app, `···` More → Statement → **CSV** (not PDF) → Custom
statement. Under Customise, leave all 7 accounts, all 3 transaction types and
all 15 categories selected, and keep **Show amounts in British Pound** on.
That last setting adds a GBP-converted column for the non-GBP wallets, which
is the only way to total across currencies without an FX table.

**Amex** — statement download, CSV with Extended Details included.

## Run

```bash
cargo run -- import bank_statements/        # bank auto-detected per file
cargo run -- import <file> --dry-run        # parse and verify, write nothing
cargo run -- inspect <file>                 # structure of an unrecognised file
cargo run -- categorise                     # apply rules
cargo run -- categorise --reset             # re-apply, keeping manual tags
cargo run -- tag <id> "<category>"          # set one by hand
cargo run -- accounts                       # accounts and date coverage
cargo run -- batches                        # import history
```

Note the `--`. Without it cargo eats `--dry-run` as its own flag.

### First run

```bash
cargo run -- import bank_statements/ --dry-run   # check before writing
cargo run -- import bank_statements/
sqlite3 spending.db < seeds/rules.sql
sqlite3 spending.db < seeds/rules_02.sql
sqlite3 spending.db < seeds/rules_03.sql
sqlite3 spending.db < seeds/rules_04.sql
cargo run -- categorise --reset
```

Order matters: `rules_02` deletes two bad rules from `rules.sql`, so a
`--reset` is needed for the transactions they wrongly claimed.

### Adding a month

```bash
cargo run -- import bank_statements/
cargo run -- categorise
```

Import is replace-by-range, scoped to (account, source, date range), so
re-importing an overlapping file is safe and a Lloyds export never touches
Amex rows from the same dates.

### Improving coverage

`categorise` prints the 15 biggest uncategorised merchants ranked by **spend**,
not count — one £400 unknown matters more than forty £2 ones. Add patterns to
a new `seeds/rules_NN.sql`, re-run it, then `categorise --reset`.

## Verifying an import

Every import recomputes the running balance from the transactions and compares
it against the file's own balance column. `balance chain verified, closing
95.11` means nothing was dropped — and that closing figure can be checked
against the summary block in the statement itself.

Three outcomes:

- **verified** — trust it.
- **BALANCE CHAIN BROKEN** — rows are missing. Check the "ROW(S) NOT PARSED"
  list first; it's usually the parser, not the bank.
- **no balance column** — Amex only. No completeness check is possible;
  reconcile against the statement total by eye.

## Query

```sql
SELECT month, category, spent_gbp, n
FROM v_category_spend ORDER BY month DESC, spent_gbp DESC;

SELECT month, account, spent_gbp, received_gbp FROM v_monthly_spend
ORDER BY month DESC;

SELECT * FROM v_uncategorised LIMIT 20;
```

Views:

| view               | what                                                   |
| ------------------ | ------------------------------------------------------ |
| `v_transactions`   | everything, joined to account, bank, category           |
| `v_spend`          | real spending: internal movements and excluded categories dropped |
| `v_monthly_spend`  | per month per account, in GBP                           |
| `v_category_spend` | per month per category                                  |
| `v_uncategorised`  | the worklist, ranked by spend                           |

Build on `v_spend`, not `v_transactions` — the difference is whether moving
money between my own accounts counts as spending.

Anything not modelled is still in `raw_json`:

```sql
SELECT json_extract(raw_json, '$.details') FROM transactions
WHERE source = 'amex_csv' AND raw_json LIKE '%Foreign Spend%' LIMIT 5;
```

## Things to look out for

**Revolut writes September as "Sept"**, four letters where every other month
gets three. chrono's `%b` wants "Sep", so every September row silently failed
to parse — 309 transactions across three years. Handled in `parse_date`, but
it's the reason skipped rows are reported: the only symptom was a chain break
a year later.

**Watch broad LIKE patterns.** `%EE%` matched SPEED QUEEN and COFFEE and put
202 rows in Bills & utilities. Check what a new pattern claims before trusting
it.

**Amex inverts the sign.** A purchase exports positive; refunds and payments
negative. Flipped at parse time so everything downstream is consistent. If a
spend total ever comes out negative, look there first.

**Amex has no running balance**, so a dropped row is undetectable. It does
supply a per-transaction Reference, which is used as the row identity — better
than the positional fallback, and stable across exports of any date range.

**Row keys must stay stable.** Manual categories hang off `row_key`, not row
id. Where a bank gives no identifier, the key is
`source:date:amount:balance:description:ordinal`, and the ordinal counts
identical rows **within a day**, never position in the file. Counting by
position would break every key when a file's date range changed.

**Revolut can emit genuinely identical rows** — same date, description, amount
and running balance, when an intervening credit restores the balance between
two identical debits. Nothing in the file distinguishes them. That's why
import replaces by range rather than merging.

**Currencies don't sum.** Use `amount_gbp` (the statement's own converted
figure, falling back to the native amount), never raw `amount_minor` across
accounts.

**Editing an applied migration** gives "migration 1 was previously applied but
has been modified". While the schema is still moving, delete the database and
re-import. Once there's data worth keeping, add `0002`.

## Layout

```
migrations/0001_init.sql    schema, indexes, triggers, views
seeds/rules*.sql            categorisation rules (gitignored, personal)
src/main.rs                 CLI
src/db.rs                   pool setup, migrations
src/importer.rs             source-agnostic database writer
src/categorise.rs           rules engine
src/statements/mod.rs       shared types, money and date parsing, registry
src/statements/revolut.rs   per-wallet sections, dual currency columns
src/statements/lloyds.rs    separate debit/credit columns, newest-first
src/statements/amex.rs      inverted signs, no balance, has a reference
```

Adding a bank means one file in `src/statements/` implementing
`StatementParser`, plus a line in `parsers()`. Nothing else changes.
