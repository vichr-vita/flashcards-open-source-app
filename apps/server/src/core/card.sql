SELECT
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
  WHERE cards.workspace_id = $1 AND cards.card_id = $2
