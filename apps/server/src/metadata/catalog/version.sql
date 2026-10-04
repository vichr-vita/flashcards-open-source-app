SELECT v.*, v.status::text AS version_status, a.author_id, a.slug AS author_slug, a.display_name AS author_display_name,
       a.bio AS author_bio, a.website_url AS author_website_url, p.slug AS package_slug,
       p.status::text AS package_status, p.delisted_at AS package_delisted_at
FROM catalog.package_versions v
JOIN catalog.packages p ON p.package_id=v.package_id
JOIN catalog.authors a ON a.author_id=p.author_id
WHERE v.package_version_id=$1
