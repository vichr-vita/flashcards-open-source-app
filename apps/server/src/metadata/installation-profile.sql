INSERT INTO analytics.installation_profiles (
    anonymous_id, platform, user_id, app_version, os_version, device_locale, timezone,
    first_seen, last_seen
  ) VALUES ($1::uuid, $2::text, $3::uuid, $4::text, $5::text, $6::text, $7::text,
    $8::timestamptz, $8::timestamptz)
  ON CONFLICT (anonymous_id, platform) DO UPDATE SET
    user_id = EXCLUDED.user_id,
    app_version = EXCLUDED.app_version,
    os_version = EXCLUDED.os_version,
    device_locale = EXCLUDED.device_locale,
    timezone = EXCLUDED.timezone,
    last_seen = EXCLUDED.last_seen
  WHERE EXCLUDED.last_seen >= installation_profiles.last_seen
    AND (
      installation_profiles.last_seen <= EXCLUDED.last_seen - INTERVAL '1 hour'
      OR (installation_profiles.user_id, installation_profiles.app_version,
          installation_profiles.os_version, installation_profiles.device_locale,
          installation_profiles.timezone)
        IS DISTINCT FROM
         (EXCLUDED.user_id, EXCLUDED.app_version, EXCLUDED.os_version,
          EXCLUDED.device_locale, EXCLUDED.timezone)
    )
