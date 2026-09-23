# Argus Panoptes

Personal notes. Imports bank statements from Lloyds, Revolut and Amex into
SQLite, categorises them, and reports on the result. Investing and health
data to follow; a GUI after that.

No banking API. Enable Banking doesn't cover the UK (EEA only, post-Brexit),
and every UK provider that does — Plaid, TrueLayer, Yapily — puts production
access behind business onboarding. Downloaded statements it is.

## Prerequisites

- Rust stable.
- A C toolchain — SQLx builds SQLite from source (`build-essential
  pkg-config` on Linux, `xcode-select --install` on macOS).
- `sqlite3` CLI for the seed files.

`.env`:

```ini
DATABASE_URL=sqlite://data/spending.db
```

`.gitignore`:

```
/target/
/data/
/seeds/
/bank_statements/
.env
```

`seeds/` is ignored because the rules contain payee names.

## Workspace

Seven crates. The split exists so a CLI build never compiles the GUI's
dependency tree — `eframe` is heavy and `cargo run -- report` should be
instant.

```
crates/
  core/       db pool, migrations, money and date parsing, row keys
  banking/    statement parsers, importer, categorisation engine
  investing/  Trading 212                            (stub)
  health/     health app ingest                      (stub)
  analytics/  every query the CLI and GUI both want
  cli/        binary: argus
  gui/        binary: argus-gui                      (stub)

data/                  spending.db
seeds/banking/         rules*.sql
bank_statements/       lloyds/ revolut/ amex/
```

`analytics` depends only on `core` — it reads tables and never writes them,
which is why it pulls in neither reqwest nor csv. Anything the GUI will show
belongs there, not in `cli`.

Migrations all live in `crates/core/migrations`, numbered by domain so the
three areas don't renumber each other: `0001_banking_*`, `0100_investing_*`,
`0200_health_*`. One database, because the interesting cross-domain questions
are joins on a date.

### The alias

```bash
mkdir -p .cargo
printf '[alias]\nargus = "run -p argus-cli --"\n' > .cargo/config.toml
```

`[alias]` only works in `.cargo/config.toml`; in `Cargo.toml` Cargo warns and
ignores it. Every command below assumes the alias — without it, use
`cargo run -p argus-cli -- <command>`.

## Getting the statements

```
bank_statements/
  lloyds/    transactions_2025_03_01-2026_03_01.csv
  revolut/   consolidated_statement_2023_10_02_2026_09_16.csv
  amex/      2026_transactions.csv
```

**Lloyds** — desktop internet banking only, not the app. Open the account,
scroll to the bottom of the transaction list, Export, pick a date range.
Roughly 12 months available. The 150-row cap people quote didn't apply: one
export came back with 717 rows.

**Revolut** — app, `···` More → Statement → **CSV** (not PDF) → Custom
statement. Under Customise, leave all 7 accounts, all 3 transaction types and
all 15 categories selected, and keep **Show amounts in British Pound** on.
That setting adds the GBP-converted column for the non-GBP wallets, which is
the only way to total across currencies without an FX table.

**Amex** — statement download, CSV with Extended Details included.

## Run

```bash
cargo argus import bank_statements/     # bank auto-detected per file
cargo argus import <file> --dry-run     # parse and verify, write nothing
cargo argus inspect <file>              # structure of an unrecognised file
cargo argus categorise                  # apply rules
cargo argus categorise --reset          # re-apply, keeping manual tags
cargo argus tag <id> "<category>"       # set one by hand
cargo argus report                      # spending breakdown
cargo argus report --months 6 --out breakdown.txt
cargo argus accounts                    # accounts and date coverage
cargo argus batches                     # import history
```

### First run

```bash
cargo argus import bank_statements/ --dry-run   # check before writing
cargo argus import bank_statements/

for f in seeds/banking/rules*.sql; do sqlite3 data/spending.db < "$f"; done

cargo argus categorise --reset
cargo argus report
```

Order matters. `rules_02` deletes two bad rules from `rules.sql`, so a
`--reset` is needed to reassign the transactions they wrongly claimed.

### Adding a month

```bash
cargo argus import bank_statements/
cargo argus categorise
cargo argus report --months 3
```

Import is replace-by-range, scoped to (account, source, date range), so
re-importing an overlapping file is safe and a Lloyds export never touches
Amex rows from the same dates.

### Improving coverage

`categorise` prints the 15 biggest uncategorised merchants ranked by **spend**,
not count — one £400 unknown matters more than forty £2 ones. Add patterns to
a new `seeds/banking/rules_NN.sql`, run it, then `categorise --reset`.

`rules_05` is different from the others: it guesses ahead at chains you
haven't hit yet, at priority 95 so every hand-written rule still wins. Some
of it will be wrong — delete what misfires.

## The report

```bash
cargo argus report
```

Six sections: overview with savings rate and coverage, monthly totals with
bars, categories with Coffee/Fast food/Drinks indented under Eating out, top
merchants, likely-recurring charges, and what's still uncategorised.

The recurring detector needs no tagging — it finds anything charged in four
or more distinct months where the amount barely moves, then totals what they
cost annually if they all keep running.

## Verifying an import

Every import recomputes the running balance and compares it against the
file's own balance column. `balance chain verified, closing 95.11` means
nothing was dropped, and that figure can be checked against the summary block
in the statement itself.

- **verified** — trust it.
- **BALANCE CHAIN BROKEN** — rows are missing. Check the "ROW(S) NOT PARSED"
  list first; it's usually the parser, not the bank.
- **no balance column** — Amex only. No completeness check is possible;
  reconcile against the statement total by eye.

## Query

```sql
SELECT month, category, spent_gbp, n
FROM v_category_spend ORDER BY month DESC, spent_gbp DESC;

SELECT * FROM v_uncategorised LIMIT 20;
```

| view               | what                                                              |
| ------------------ | ----------------------------------------------------------------- |
| `v_transactions`   | everything, joined to account, bank, category                      |
| `v_spend`          | real spending: internal movements and excluded categories dropped  |
| `v_monthly_spend`  | per month per account, in GBP                                      |
| `v_category_spend` | per month per category                                             |
| `v_uncategorised`  | the worklist, ranked by spend                                      |

Build on `v_spend`, not `v_transactions` — the difference is whether moving
money between my own accounts counts as spending.

Anything not modelled is still in `raw_json`:

```sql
SELECT json_extract(raw_json, '$.details') FROM transactions
WHERE source = 'amex_csv' AND raw_json LIKE '%Foreign Spend%' LIMIT 5;
```

## Things to look out for

**Lower priority wins.** A broad pattern at 10-20 outranks every merchant
rule. `%MARCUS%` for the savings bank claimed "Brother Marcus" before the
Eating out rule at 90 could — narrowed to `%MARCUS BY GOLDMAN%`. Worth
remembering for anything else added low.

**Revolut writes September as "Sept"**, four letters where every other month
gets three. chrono's `%b` wants "Sep", so every September row silently failed
to parse — 309 transactions across three years. Fixed, but it's why skipped
rows are reported: the only symptom was a chain break a year later.

**Watch broad LIKE patterns.** `%EE%` matched SPEED QUEEN and COFFEE and put
202 rows in Bills & utilities.

**Amex inverts the sign.** A purchase exports positive; refunds and payments
negative. Flipped at parse time. If a spend total comes out negative, look
there first.

**Amex has no running balance**, so a dropped row is undetectable. It does
supply a per-transaction Reference, used as the row identity.

**Row keys must stay stable.** Manual categories hang off `row_key`, not row
id. Where a bank gives no identifier the key is
`source:date:amount:balance:description:ordinal`, and the ordinal counts
identical rows **within a day**, never position in the file.

**Revolut can emit genuinely identical rows** — same date, description,
amount and running balance, when an intervening credit restores the balance
between two identical debits. That's why import replaces by range rather than
merging.

**Currencies don't sum.** Use `COALESCE(base_amount_minor, amount_minor)`,
never raw `amount_minor` across accounts.

**Editing an applied migration** gives "migration 1 was previously applied but
has been modified". While the schema is moving, delete the database and
re-import. Once there's data worth keeping, add a new numbered file.
