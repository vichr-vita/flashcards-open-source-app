WITH review_event_local_dates AS (
SELECT
COALESCE(
review_events.reviewed_local_date,
timezone(COALESCE(review_events.reviewed_time_zone, $3), review_events.reviewed_at_client)::date
) AS review_date,
review_events.rating
FROM content.review_events AS review_events
WHERE review_events.workspace_id = $1
AND review_events.reviewed_by_user_id = $2
AND review_events.reviewed_at_client >= (($4::date - 3)::timestamp AT TIME ZONE $3)
AND review_events.reviewed_at_client < (($5::date + 3)::timestamp AT TIME ZONE $3)
)
SELECT
to_char(review_event_local_dates.review_date, 'YYYY-MM-DD') AS review_date,
COUNT(*)::int AS review_count,
COUNT(*) FILTER (WHERE review_event_local_dates.rating = 0)::int AS again_count,
COUNT(*) FILTER (WHERE review_event_local_dates.rating = 1)::int AS hard_count,
COUNT(*) FILTER (WHERE review_event_local_dates.rating = 2)::int AS good_count,
COUNT(*) FILTER (WHERE review_event_local_dates.rating = 3)::int AS easy_count
FROM review_event_local_dates
WHERE review_event_local_dates.review_date BETWEEN $4::date AND $5::date
GROUP BY review_event_local_dates.review_date
ORDER BY review_event_local_dates.review_date ASC
