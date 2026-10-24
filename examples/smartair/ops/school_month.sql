-- One month of hourly rollups for one school -> the figures in its parent report.
-- Thresholds: Bangladesh Air Pollution Control Rules 2022 (PM2.5 24 h limit 65, annual 35 µg/m³)
-- and the WHO 2021 24 h guideline (15 µg/m³). Readings are fixed point x10.
CREATE TABLE school_month AS
SELECT
    device_id,
    room,
    count(*)                                                        AS hours_measured,
    round(avg(indoor_pm25_ugm3_x10) / 10.0, 1)                      AS indoor_mean_ugm3,
    round(avg(outdoor_pm25_ugm3_x10) / 10.0, 1)                     AS outdoor_mean_ugm3,
    round(avg(indoor_pm25_ugm3_x10) / nullif(avg(outdoor_pm25_ugm3_x10), 0), 2) AS indoor_outdoor_ratio,
    count(*) FILTER (WHERE indoor_pm25_ugm3_x10 > 350)              AS hours_above_35,
    count(*) FILTER (WHERE indoor_pm25_ugm3_x10 > 150)              AS hours_above_who_15
FROM hourly
WHERE school_hours
GROUP BY ALL
ORDER BY room;
