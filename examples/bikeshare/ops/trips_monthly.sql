-- Trip files for one month -> trips per station per day (sql operator, DuckDB dialect).
-- Input table `trips` is the month's files from the public archive; output `trips_daily`.
-- Rows are kept as published; trips under a minute or over a day are counted apart, not dropped, so the raw
-- and the cleaned figures both stay in lineage.
CREATE TABLE trips_daily AS
SELECT
    start_station_id                                  AS station_id,
    CAST(date_trunc('day', started_at) AS DATE)       AS day,
    count(*) FILTER (WHERE duration_s BETWEEN 60 AND 86400) AS trips,
    count(*) FILTER (WHERE duration_s < 60 OR duration_s > 86400) AS excluded,
    round(avg(duration_s) FILTER (WHERE duration_s BETWEEN 60 AND 86400) / 60.0, 1) AS avg_duration_min,
    round(avg(CAST(member AS INTEGER)), 2)            AS member_share
FROM trips
WHERE start_station_id IS NOT NULL
GROUP BY ALL
ORDER BY station_id, day;
