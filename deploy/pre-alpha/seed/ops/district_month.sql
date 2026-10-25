-- One month of hourly rollups -> the figures of a district's service report.
-- Availability counts an hour as served when a station had at least one bike to rent and one dock to return to.
-- Readings are the rollup columns of `dock-telemetry`; the district comes from the station's id.
CREATE TABLE district_month AS
SELECT
    station_id,
    district,
    count(*)                                                       AS hours_measured,
    round(avg(bikes_mean_x100) / 100.0, 1)                         AS avg_bikes,
    round(avg(capacity - bikes_mean_x100 / 100.0), 1)              AS avg_docks_free,
    round(count(*) FILTER (WHERE bikes_min >= 1 AND capacity - bikes_max >= 1) * 1.0 / count(*), 2) AS availability,
    count(*) FILTER (WHERE bikes_max = 0)                          AS hours_empty,
    count(*) FILTER (WHERE bikes_min = capacity)                   AS hours_full
FROM hourly
WHERE service_hour
GROUP BY ALL
ORDER BY district, station_id;
