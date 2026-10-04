WITH latest AS (
  SELECT DISTINCT ON(package_id) * FROM catalog.package_versions
  WHERE status='published' AND delisted_at IS NULL ORDER BY package_id, version_number DESC
)
SELECT v.*, v.status::text AS version_status, p.slug AS package_slug,
       a.author_id, a.slug AS author_slug, a.display_name AS author_display_name,
       a.bio AS author_bio, a.website_url AS author_website_url
FROM latest v JOIN catalog.packages p USING(package_id) JOIN catalog.authors a ON a.author_id=p.author_id
WHERE p.status='published' AND p.delisted_at IS NULL
  AND ($1::text IS NULL OR lower(p.slug) LIKE $1 ESCAPE '\' OR lower(v.title) LIKE $1 ESCAPE '\'
    OR lower(v.summary) LIKE $1 ESCAPE '\' OR lower(v.description) LIKE $1 ESCAPE '\'
    OR lower(a.display_name) LIKE $1 ESCAPE '\')
  AND ($2::text IS NULL OR $2=ANY(v.language_tags))
ORDER BY v.published_at DESC NULLS LAST, v.package_version_id DESC LIMIT $3
