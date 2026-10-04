WITH target_review_events AS (
SELECT
review_events.review_event_id,
review_events.reviewed_by_user_id,
review_events.reviewed_at_client,
review_events.reviewed_local_date,
COALESCE(review_events.reviewed_time_zone, $2) AS materialized_time_zone,
COALESCE(review_events.reviewed_time_zone_source, 'user_settings') AS materialized_time_zone_source,
timezone(COALESCE(review_events.reviewed_time_zone, $2), review_events.reviewed_at_client)::date
AS materialized_local_date,
active_days.local_date AS active_day_local_date
FROM content.review_events AS review_events
LEFT JOIN progress.user_active_review_days AS active_days
ON active_days.reviewed_by_user_id = review_events.reviewed_by_user_id
AND active_days.local_date = COALESCE(
review_events.reviewed_local_date,
timezone(COALESCE(review_events.reviewed_time_zone, $2), review_events.reviewed_at_client)::date
)
WHERE review_events.reviewed_by_user_id = $1
AND review_events.workspace_id = $3
AND (
review_events.reviewed_local_date IS NULL
OR active_days.local_date IS NULL
)
), updated_review_events AS (
UPDATE content.review_events AS review_events
SET reviewed_time_zone = target_review_events.materialized_time_zone,
reviewed_local_date = target_review_events.materialized_local_date,
reviewed_time_zone_source = target_review_events.materialized_time_zone_source
FROM target_review_events
WHERE review_events.review_event_id = target_review_events.review_event_id
AND target_review_events.reviewed_local_date IS NULL
RETURNING review_events.review_event_id
), active_day_rows AS (
SELECT
target_review_events.reviewed_by_user_id,
COALESCE(target_review_events.reviewed_local_date, target_review_events.materialized_local_date) AS local_date,
COUNT(*)::int AS review_count,
MIN(target_review_events.reviewed_at_client) AS first_reviewed_at_client,
MAX(target_review_events.reviewed_at_client) AS last_reviewed_at_client,
(ARRAY_AGG(
target_review_events.materialized_time_zone
ORDER BY target_review_events.reviewed_at_client ASC, target_review_events.review_event_id ASC
))[1] AS time_zone,
(ARRAY_AGG(
target_review_events.materialized_time_zone_source
ORDER BY target_review_events.reviewed_at_client ASC, target_review_events.review_event_id ASC
))[1] AS time_zone_source
FROM target_review_events
GROUP BY
target_review_events.reviewed_by_user_id,
COALESCE(target_review_events.reviewed_local_date, target_review_events.materialized_local_date)
), upserted_active_days AS (
INSERT INTO progress.user_active_review_days
(
reviewed_by_user_id, local_date, review_count, first_reviewed_at_client,
last_reviewed_at_client, time_zone, time_zone_source
)
SELECT
active_day_rows.reviewed_by_user_id,
active_day_rows.local_date,
active_day_rows.review_count,
active_day_rows.first_reviewed_at_client,
active_day_rows.last_reviewed_at_client,
active_day_rows.time_zone,
active_day_rows.time_zone_source
FROM active_day_rows
ON CONFLICT (reviewed_by_user_id, local_date) DO UPDATE
SET
review_count = progress.user_active_review_days.review_count + EXCLUDED.review_count,
first_reviewed_at_client = LEAST(progress.user_active_review_days.first_reviewed_at_client, EXCLUDED.first_reviewed_at_client),
last_reviewed_at_client = GREATEST(progress.user_active_review_days.last_reviewed_at_client, EXCLUDED.last_reviewed_at_client),
time_zone = CASE
WHEN EXCLUDED.first_reviewed_at_client < progress.user_active_review_days.first_reviewed_at_client THEN EXCLUDED.time_zone
ELSE progress.user_active_review_days.time_zone
END,
time_zone_source = CASE
WHEN EXCLUDED.first_reviewed_at_client < progress.user_active_review_days.first_reviewed_at_client THEN EXCLUDED.time_zone_source
ELSE progress.user_active_review_days.time_zone_source
END,
updated_at = now()
RETURNING reviewed_by_user_id, local_date
)
SELECT
(SELECT COUNT(*)::int FROM updated_review_events) AS review_events_materialized,
(SELECT COUNT(*)::int FROM upserted_active_days) AS active_review_days_upserted
