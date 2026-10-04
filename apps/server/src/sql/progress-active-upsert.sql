INSERT INTO progress.user_active_review_days
(
reviewed_by_user_id, local_date, review_count, first_reviewed_at_client,
last_reviewed_at_client, time_zone, time_zone_source
)
VALUES ($1, $2::date, 1, $3, $3, $4, $5)
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
