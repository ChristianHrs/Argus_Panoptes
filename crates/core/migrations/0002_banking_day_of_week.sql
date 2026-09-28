-- Daily and day-of-week spending.
------------------------------------------------------------------------------
-- One row per calendar day in the data's range, spending days or not.
------------------------------------------------------------------------------
CREATE VIEW v_daily_spend AS
WITH RECURSIVE
bounds AS (
    SELECT MIN(booking_date) AS first_day,
           MAX(booking_date) AS last_day
      FROM v_spend
),
calendar(day) AS (
    SELECT first_day FROM bounds
    UNION ALL
    SELECT date(day, '+1 day') FROM calendar, bounds WHERE day < last_day
)
SELECT
    c.day,
    CAST(strftime('%w', c.day) AS INTEGER) AS dow,
    -- Monday first; SQLite numbers Sunday as 0.
    CASE strftime('%w', c.day)
        WHEN '1' THEN 1 WHEN '2' THEN 2 WHEN '3' THEN 3 WHEN '4' THEN 4
        WHEN '5' THEN 5 WHEN '6' THEN 6 ELSE 7
    END AS dow_order,
    CASE strftime('%w', c.day)
        WHEN '1' THEN 'Monday'   WHEN '2' THEN 'Tuesday' WHEN '3' THEN 'Wednesday'
        WHEN '4' THEN 'Thursday' WHEN '5' THEN 'Friday'  WHEN '6' THEN 'Saturday'
        ELSE 'Sunday'
    END AS day_name,
    substr(c.day, 1, 7) AS month,
    ROUND(COALESCE(SUM(CASE WHEN s.amount_gbp < 0 THEN -s.amount_gbp END), 0), 2) AS spent_gbp,
    COUNT(s.id) AS n
FROM calendar c
LEFT JOIN v_spend s ON s.booking_date = c.day
GROUP BY c.day;


------------------------------------------------------------------------------
-- Average and median spend by day of the week.
------------------------------------------------------------------------------
CREATE VIEW v_day_of_week AS
WITH ranked AS (
    SELECT dow_order,
           day_name,
           spent_gbp,
           n,
           ROW_NUMBER() OVER (PARTITION BY dow_order ORDER BY spent_gbp) AS rn,
           COUNT(*)     OVER (PARTITION BY dow_order)                    AS days
      FROM v_daily_spend
),
median_day AS (
    SELECT dow_order, ROUND(AVG(spent_gbp), 2) AS median_gbp
      FROM ranked
     WHERE rn IN ((days + 1) / 2, (days + 2) / 2)
     GROUP BY dow_order
),
-- Per-transaction median, over spending days only: a day with no
-- transactions has no transaction size to contribute.
txn_ranked AS (
    SELECT CASE strftime('%w', booking_date)
               WHEN '1' THEN 1 WHEN '2' THEN 2 WHEN '3' THEN 3 WHEN '4' THEN 4
               WHEN '5' THEN 5 WHEN '6' THEN 6 ELSE 7
           END AS dow_order,
           -amount_gbp AS amount,
           ROW_NUMBER() OVER (
               PARTITION BY CASE strftime('%w', booking_date)
                   WHEN '1' THEN 1 WHEN '2' THEN 2 WHEN '3' THEN 3 WHEN '4' THEN 4
                   WHEN '5' THEN 5 WHEN '6' THEN 6 ELSE 7 END
               ORDER BY -amount_gbp
           ) AS rn,
           COUNT(*) OVER (
               PARTITION BY CASE strftime('%w', booking_date)
                   WHEN '1' THEN 1 WHEN '2' THEN 2 WHEN '3' THEN 3 WHEN '4' THEN 4
                   WHEN '5' THEN 5 WHEN '6' THEN 6 ELSE 7 END
           ) AS txns
      FROM v_spend
     WHERE amount_gbp < 0
),
median_txn AS (
    SELECT dow_order, ROUND(AVG(amount), 2) AS median_txn_gbp
      FROM txn_ranked
     WHERE rn IN ((txns + 1) / 2, (txns + 2) / 2)
     GROUP BY dow_order
)
SELECT
    r.dow_order,
    r.day_name,
    COUNT(*)                              AS days_observed,
    SUM(CASE WHEN r.n > 0 THEN 1 ELSE 0 END) AS days_with_spending,
    SUM(r.n)                              AS transactions,
    ROUND(SUM(r.spent_gbp), 2)            AS total_gbp,
    ROUND(AVG(r.spent_gbp), 2)            AS avg_per_day_gbp,
    md.median_gbp                         AS median_per_day_gbp,
    ROUND(MAX(r.spent_gbp), 2)            AS busiest_day_gbp,
    ROUND(SUM(r.spent_gbp) / NULLIF(SUM(r.n), 0), 2) AS avg_per_txn_gbp,
    mt.median_txn_gbp                     AS median_txn_gbp
FROM ranked r
LEFT JOIN median_day md ON md.dow_order = r.dow_order
LEFT JOIN median_txn mt ON mt.dow_order = r.dow_order
GROUP BY r.dow_order, r.day_name, md.median_gbp, mt.median_txn_gbp
ORDER BY r.dow_order;


------------------------------------------------------------------------------
-- Day of week crossed with category, for the follow-up question: is Saturday
-- expensive because of groceries or because of drinks?
------------------------------------------------------------------------------
CREATE VIEW v_day_of_week_category AS
SELECT
    CASE strftime('%w', booking_date)
        WHEN '1' THEN 1 WHEN '2' THEN 2 WHEN '3' THEN 3 WHEN '4' THEN 4
        WHEN '5' THEN 5 WHEN '6' THEN 6 ELSE 7
    END AS dow_order,
    CASE strftime('%w', booking_date)
        WHEN '1' THEN 'Monday'   WHEN '2' THEN 'Tuesday' WHEN '3' THEN 'Wednesday'
        WHEN '4' THEN 'Thursday' WHEN '5' THEN 'Friday'  WHEN '6' THEN 'Saturday'
        ELSE 'Sunday'
    END AS day_name,
    COALESCE(category, 'Uncategorised') AS category,
    COUNT(*) AS n,
    ROUND(SUM(-amount_gbp), 2) AS total_gbp
FROM v_spend
WHERE amount_gbp < 0
GROUP BY dow_order, day_name, COALESCE(category, 'Uncategorised')
ORDER BY dow_order, total_gbp DESC;
