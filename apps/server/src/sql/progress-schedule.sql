WITH schedule_boundaries AS (
SELECT
((timezone($2, $3::timestamptz)::date + 1)::timestamp AT TIME ZONE $2) AS tomorrow_start,
((timezone($2, $3::timestamptz)::date + 8)::timestamp AT TIME ZONE $2) AS days_8_start,
((timezone($2, $3::timestamptz)::date + 31)::timestamp AT TIME ZONE $2) AS days_31_start,
((timezone($2, $3::timestamptz)::date + 91)::timestamp AT TIME ZONE $2) AS days_91_start,
((timezone($2, $3::timestamptz)::date + 361)::timestamp AT TIME ZONE $2) AS days_361_start,
((timezone($2, $3::timestamptz)::date + 721)::timestamp AT TIME ZONE $2) AS days_721_start
)
SELECT
COUNT(*) FILTER (WHERE cards.due_at IS NULL)::int AS new_count,
COUNT(*) FILTER (WHERE cards.due_at IS NOT NULL AND cards.due_at < schedule_boundaries.tomorrow_start)::int AS today_count,
COUNT(*) FILTER (WHERE cards.due_at >= schedule_boundaries.tomorrow_start AND cards.due_at < schedule_boundaries.days_8_start)::int AS days_1_to_7_count,
COUNT(*) FILTER (WHERE cards.due_at >= schedule_boundaries.days_8_start AND cards.due_at < schedule_boundaries.days_31_start)::int AS days_8_to_30_count,
COUNT(*) FILTER (WHERE cards.due_at >= schedule_boundaries.days_31_start AND cards.due_at < schedule_boundaries.days_91_start)::int AS days_31_to_90_count,
COUNT(*) FILTER (WHERE cards.due_at >= schedule_boundaries.days_91_start AND cards.due_at < schedule_boundaries.days_361_start)::int AS days_91_to_360_count,
COUNT(*) FILTER (WHERE cards.due_at >= schedule_boundaries.days_361_start AND cards.due_at < schedule_boundaries.days_721_start)::int AS years_1_to_2_count,
COUNT(*) FILTER (WHERE cards.due_at >= schedule_boundaries.days_721_start)::int AS later_count
FROM content.cards AS cards
CROSS JOIN schedule_boundaries
WHERE cards.workspace_id = $1 AND cards.deleted_at IS NULL
