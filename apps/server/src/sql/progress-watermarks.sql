WITH requested_workspaces AS (
SELECT requested_workspace_ids.workspace_id
FROM unnest($1::uuid[]) AS requested_workspace_ids(workspace_id)
WHERE security.current_workspace_access_allowed(requested_workspace_ids.workspace_id)
)
SELECT
requested_workspaces.workspace_id::text AS workspace_id,
COALESCE(MAX(review_events.review_sequence), 0) AS review_sequence_id
FROM requested_workspaces
LEFT JOIN content.review_events AS review_events
ON review_events.workspace_id = requested_workspaces.workspace_id
GROUP BY requested_workspaces.workspace_id
ORDER BY requested_workspaces.workspace_id ASC
