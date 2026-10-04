WITH bootstrap_entries AS (
  SELECT
    0 AS entity_rank,
    'workspace_scheduler_settings'::text AS entity_type,
    workspaces.workspace_id::text AS entity_id,
    jsonb_build_object(
      'algorithm', workspaces.fsrs_algorithm,
      'desiredRetention', workspaces.fsrs_desired_retention,
      'learningStepsMinutes', workspaces.fsrs_learning_steps_minutes,
      'relearningStepsMinutes', workspaces.fsrs_relearning_steps_minutes,
      'maximumIntervalDays', workspaces.fsrs_maximum_interval_days,
      'enableFuzz', workspaces.fsrs_enable_fuzz,
      'clientUpdatedAt', to_char(date_trunc('milliseconds', workspaces.fsrs_client_updated_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
      'lastModifiedByReplicaId', workspaces.fsrs_last_modified_by_replica_id::text,
      'lastOperationId', workspaces.fsrs_last_operation_id,
      'updatedAt', to_char(date_trunc('milliseconds', workspaces.fsrs_updated_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')
    ) AS payload
  FROM org.workspaces AS workspaces
  WHERE workspaces.workspace_id = $1
  UNION ALL
  SELECT
    1 AS entity_rank,
    'card'::text AS entity_type,
    cards.card_id::text AS entity_id,
    jsonb_build_object(
      'cardId', cards.card_id::text,
      'frontText', cards.front_text,
      'backText', cards.back_text,
      'cardType', CASE WHEN btrim(cards.card_type) = '' THEN 'basic' ELSE cards.card_type END,
      'metadata', cards.metadata,
      'tags', cards.tags,
      'effortLevel', 'fast',
      'dueAt', CASE WHEN cards.due_at IS NULL THEN NULL ELSE to_char(date_trunc('milliseconds', cards.due_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') END,
      'createdAt', to_char(date_trunc('milliseconds', cards.created_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
      'reps', cards.reps,
      'lapses', cards.lapses,
      'fsrsCardState', cards.fsrs_card_state,
      'fsrsStepIndex', cards.fsrs_step_index,
      'fsrsStability', cards.fsrs_stability,
      'fsrsDifficulty', cards.fsrs_difficulty,
      'fsrsLastReviewedAt', CASE WHEN cards.fsrs_last_reviewed_at IS NULL THEN NULL ELSE to_char(date_trunc('milliseconds', cards.fsrs_last_reviewed_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') END,
      'fsrsScheduledDays', cards.fsrs_scheduled_days,
      'clientUpdatedAt', to_char(date_trunc('milliseconds', cards.client_updated_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
      'lastModifiedByReplicaId', cards.last_modified_by_replica_id::text,
      'lastOperationId', cards.last_operation_id,
      'updatedAt', to_char(date_trunc('milliseconds', cards.updated_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
      'deletedAt', CASE WHEN cards.deleted_at IS NULL THEN NULL ELSE to_char(date_trunc('milliseconds', cards.deleted_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') END
    ) AS payload
  FROM content.cards AS cards
  WHERE cards.workspace_id = $1
  UNION ALL
  SELECT
    2 AS entity_rank,
    'deck'::text AS entity_type,
    decks.deck_id::text AS entity_id,
    jsonb_build_object(
      'deckId', decks.deck_id::text,
      'workspaceId', decks.workspace_id::text,
      'name', decks.name,
      'filterDefinition', jsonb_build_object(
        'version', 2,
        'effortLevels', '[]'::jsonb,
        'tags', decks.filter_definition->'tags'
      ),
      'createdAt', to_char(date_trunc('milliseconds', decks.created_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
      'clientUpdatedAt', to_char(date_trunc('milliseconds', decks.client_updated_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
      'lastModifiedByReplicaId', decks.last_modified_by_replica_id::text,
      'lastOperationId', decks.last_operation_id,
      'updatedAt', to_char(date_trunc('milliseconds', decks.updated_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
      'deletedAt', CASE WHEN decks.deleted_at IS NULL THEN NULL ELSE to_char(date_trunc('milliseconds', decks.deleted_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') END
    ) AS payload
  FROM content.decks AS decks
  WHERE decks.workspace_id = $1
  UNION ALL
  SELECT
    3 AS entity_rank,
    'media_asset'::text AS entity_type,
    media_assets.media_asset_id::text AS entity_id,
    jsonb_build_object(
      'mediaAssetId', media_assets.media_asset_id::text,
      'workspaceId', media_assets.workspace_id::text,
      'mimeType', media_blobs.mime_type,
      'sizeBytes', media_blobs.size_bytes,
      'sha256', media_blobs.sha256,
      'sourceUrl', media_assets.source_url,
      'createdAt', to_char(date_trunc('milliseconds', media_assets.created_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
      'clientUpdatedAt', to_char(date_trunc('milliseconds', media_assets.client_updated_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
      'lastModifiedByReplicaId', media_assets.last_modified_by_replica_id::text,
      'lastOperationId', media_assets.last_operation_id,
      'updatedAt', to_char(date_trunc('milliseconds', media_assets.updated_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
      'deletedAt', CASE WHEN media_assets.deleted_at IS NULL THEN NULL ELSE to_char(date_trunc('milliseconds', media_assets.deleted_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') END
    ) AS payload
  FROM content.media_assets AS media_assets
  INNER JOIN content.media_blobs AS media_blobs
  ON media_blobs.media_blob_id = media_assets.media_blob_id
  WHERE media_assets.workspace_id = $1
  AND $2::boolean
)
SELECT entity_rank, entity_type, entity_id, payload FROM bootstrap_entries ORDER BY entity_rank ASC, entity_id ASC
