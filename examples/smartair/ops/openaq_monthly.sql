-- OpenAQ archive rows for one month -> daily PM2.5 per location (sql operator, DuckDB dialect).
-- Input table `measurements` is the month's csv.gz files; output `daily_pm25`.
-- Values stay as published; the reference-correction step is a separate, hashed operator so raw
-- and corrected values both remain in lineage.
CREATE TABLE daily_pm25 AS
SELECT
    location_id,
    CAST(date_trunc('day', datetime) AS DATE) AS day,
    count(*)                                   AS readings,
    round(avg(value), 1)                       AS pm25_mean_ugm3,
    max(value)                                 AS pm25_max_ugm3
FROM measurements
WHERE parameter = 'pm25' AND value >= 0 AND value < 2000
GROUP BY ALL
ORDER BY location_id, day;
