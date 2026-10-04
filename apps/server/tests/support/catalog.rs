#![allow(
    clippy::indexing_slicing,
    reason = "The fixture asserts known JSON shapes from real HTTP responses."
)]
use super::{call, migrations, set};
use color_eyre::eyre::{Result, eyre};
use futures_util::FutureExt as _;
use reqwest::{Client, Method};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    author: Uuid,
    package: Uuid,
    version: Uuid,
    blob: Uuid,
    workspace: Uuid,
    replica: Uuid,
}

async fn fixture(
    client: &Client,
    base: &str,
    owner: &PgPool,
    user: Uuid,
    token: &str,
    csrf: &str,
) -> Result<Fixture> {
    let (status, workspace) = call(
        client,
        base,
        "/v1/workspaces",
        Method::POST,
        token,
        csrf,
        Some(json!({"name":"Catalog preservation"})),
    )
    .await?;
    assert_eq!(status, 201, "{workspace}");
    let workspace = workspace
        .pointer("/workspace/workspaceId")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("Workspace missing"))?
        .parse()?;
    let f = Fixture {
        author: Uuid::new_v4(),
        package: Uuid::new_v4(),
        version: Uuid::new_v4(),
        blob: Uuid::new_v4(),
        workspace,
        replica: Uuid::new_v4(),
    };
    let mut tx = owner.begin().await?;
    sqlx::query("INSERT INTO sync.workspace_replicas(replica_id,workspace_id,user_id,actor_kind,actor_key,platform) VALUES($1,$2,$3,'workspace_seed','catalog-integration','system')").bind(f.replica).bind(f.workspace).bind(user.to_string()).execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO catalog.authors(author_id,slug,display_name) VALUES($1,$2,'Fixture author')",
    )
    .bind(f.author)
    .bind(format!("fixture-{}", f.author))
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO catalog.packages(package_id,author_id,slug,title,summary,description,language_tags,license,status,educational_subject) VALUES($1,$2,$3,'Fixture title','Fixture summary','Fixture description',ARRAY['en'],'CC0-1.0','published','Testing')").bind(f.package).bind(f.author).bind(format!("fixture-{}",f.package)).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO catalog.package_versions(package_version_id,package_id,version_number,slug,title,summary,description,language_tags,license,card_count,created_by_admin_email,educational_subject) SELECT $1,package_id,1,slug,title,summary,description,language_tags,license,2,'fixture@example.invalid',educational_subject FROM catalog.packages WHERE package_id=$2").bind(f.version).bind(f.package).execute(&mut *tx).await?;
    let digest = format!("{:x}", Sha256::digest(f.blob.as_bytes()));
    sqlx::query("INSERT INTO content.media_blobs(media_blob_id,sha256,mime_type,size_bytes,storage_key) VALUES($1,$2,'image/png',32,$3)").bind(f.blob).bind(&digest).bind(format!("media/blobs/sha256/{}/{}/{digest}",digest.get(..2).ok_or_else(||eyre!("Digest prefix missing"))?,digest.get(2..4).ok_or_else(||eyre!("Digest prefix missing"))?)).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO catalog.package_media_assets(package_media_asset_id,package_id,package_version_id,package_media_key,media_blob_id) VALUES($1,$2,$3,'diagram',$4)").bind(Uuid::new_v4()).bind(f.package).bind(f.version).bind(f.blob).execute(&mut *tx).await?;
    let markdown = "![diagram](fcasset:diagram)\n\n![titled](fcasset:diagram \"caption ]( literal\")\n\n![escaped](fcasset\\:diagram)\n\n![entity](fcasset&#58;diagram)\n\n[reference][figure]\n\n[figure]: fcasset\\:diagram \"caption\"\n\n`![code](fcasset:missing)`\n\n```md\n![code](fcasset:missing)\n```";
    sqlx::query("INSERT INTO catalog.package_cards(package_card_id,package_version_id,stable_card_key,ordinal,front_text,back_text,card_type,metadata,tags,media_asset_keys) VALUES($1,$2,'one',1,'Question',$3,'basic','{\"version\":1,\"source\":null}',ARRAY['﻿keep','remove','keep','Ｚ','😀'],ARRAY['diagram']),($4,$2,'two',2,'Second question','Second answer','basic','{\"version\":1,\"source\":null}',ARRAY['keep'],ARRAY[]::text[])")
        .bind(Uuid::new_v4()).bind(f.version).bind(markdown).bind(Uuid::new_v4()).execute(&mut *tx).await?;
    for status in ["submitted", "approved", "published"] {
        sqlx::query("UPDATE catalog.package_versions SET status=$1::catalog.package_status,published_at=CASE WHEN $1='published' THEN now() ELSE NULL END WHERE package_version_id=$2").bind(status).bind(f.version).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(f)
}

async fn public_reads(
    client: &Client,
    base: &str,
    f: &Fixture,
    token: &str,
    csrf: &str,
) -> Result<()> {
    let list: Value = client
        .get(format!(
            "{base}/v1/catalog/packages?q=fixture&languageTag=en"
        ))
        .send()
        .await?
        .json()
        .await?;
    assert!(
        list["catalogPackages"]
            .as_array()
            .is_some_and(|values| values.iter().any(|v| v["packageId"] == json!(f.package))),
        "{list}"
    );
    let read: Value = client
        .get(format!(
            "{base}/v1/catalog/package-versions/{}/cards",
            f.version
        ))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(read["cards"].as_array().map(Vec::len), Some(2), "{read}");
    let (status, preview) = call(
        client,
        base,
        &format!(
            "/v1/workspaces/{}/catalog/package-versions/{}/install/preview",
            f.workspace, f.version
        ),
        Method::POST,
        token,
        csrf,
        None,
    )
    .await?;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview.pointer("/tagCounts/0/cardsCount"), Some(&json!(3)));
    assert_eq!(
        preview.pointer("/defaultOptions/suggestedImportTag"),
        Some(&json!(format!(
            "import:{}-0",
            chrono::Utc::now().format("%Y-%m-%d")
        )))
    );
    let snapshot = client.get(format!("{base}/v1/catalog")).send().await?;
    assert_eq!(snapshot.status(), 503);
    Ok(())
}

async fn private_markdown(client: &Client, base: &str, owner: &PgPool, f: &Fixture) -> Result<()> {
    let version = Uuid::new_v4();
    sqlx::query("INSERT INTO catalog.package_versions(package_version_id,package_id,version_number,slug,title,summary,description,language_tags,license,card_count,created_by_admin_email,educational_subject) SELECT $1,package_id,2,slug,title,summary,description,language_tags,license,1,'fixture@example.invalid',educational_subject FROM catalog.packages WHERE package_id=$2").bind(version).bind(f.package).execute(owner).await?;
    let digest = format!("{:x}", Sha256::digest(f.blob.as_bytes()));
    let markdown = format!("![diagram](fcasset:diagram \"media/blobs/sha256/{digest}\")");
    sqlx::query("INSERT INTO catalog.package_cards(package_card_id,package_version_id,stable_card_key,ordinal,front_text,back_text,card_type,metadata,tags,media_asset_keys) VALUES($1,$2,'private',1,'Question',$3,'basic','{\"version\":1,\"source\":null}',ARRAY[]::text[],ARRAY[]::text[])").bind(Uuid::new_v4()).bind(version).bind(markdown).execute(owner).await?;
    for status in ["submitted", "approved", "published"] {
        sqlx::query("UPDATE catalog.package_versions SET status=$1::catalog.package_status,published_at=CASE WHEN $1='published' THEN now() ELSE NULL END WHERE package_version_id=$2").bind(status).bind(version).execute(owner).await?;
    }
    let response = client
        .get(format!(
            "{base}/v1/catalog/package-versions/{version}/cards"
        ))
        .send()
        .await?;
    assert_eq!(response.status(), 409);
    let rejected: Value = response.json().await?;
    assert_eq!(
        rejected.get("code"),
        Some(&json!("CATALOG_PUBLIC_MEDIA_KEY_NOT_PUBLIC"))
    );
    Ok(())
}

async fn invalid_provenance(
    client: &Client,
    base: &str,
    owner: &PgPool,
    f: &Fixture,
    token: &str,
    csrf: &str,
) -> Result<()> {
    for (number, source) in [
        (3, json!({})),
        (
            4,
            json!({"label":null,"author":null,"comment":null,"createdAt":42,"importedAt":null,"importId":null}),
        ),
    ] {
        let version = Uuid::new_v4();
        sqlx::query("INSERT INTO catalog.package_versions(package_version_id,package_id,version_number,slug,title,summary,description,language_tags,license,card_count,created_by_admin_email,educational_subject) SELECT $1,package_id,$3,slug,title,summary,description,language_tags,license,1,'fixture@example.invalid',educational_subject FROM catalog.packages WHERE package_id=$2").bind(version).bind(f.package).bind(number).execute(owner).await?;
        sqlx::query("INSERT INTO catalog.package_media_assets(package_media_asset_id,package_id,package_version_id,package_media_key,media_blob_id) VALUES($1,$2,$3,'diagram',$4)").bind(Uuid::new_v4()).bind(f.package).bind(version).bind(f.blob).execute(owner).await?;
        sqlx::query("INSERT INTO catalog.package_cards(package_card_id,package_version_id,stable_card_key,ordinal,front_text,back_text,card_type,metadata,tags,media_asset_keys) VALUES($1,$2,'invalid-source',1,'Question','![diagram](fcasset:diagram)','basic',$3,ARRAY[]::text[],ARRAY['diagram'])").bind(Uuid::new_v4()).bind(version).bind(json!({"version":1,"source":source})).execute(owner).await?;
        for status in ["submitted", "approved", "published"] {
            sqlx::query("UPDATE catalog.package_versions SET status=$1::catalog.package_status,published_at=CASE WHEN $1='published' THEN now() ELSE NULL END WHERE package_version_id=$2").bind(status).bind(version).execute(owner).await?;
        }
        let path = format!(
            "/v1/workspaces/{}/catalog/package-versions/{version}/install",
            f.workspace
        );
        let input = json!({"installId":format!("invalid-source-{number}"),"installedAt":"2026-06-01T12:00:00.000Z","clientUpdatedAt":"2026-06-01T12:00:00.000Z","lastModifiedByReplicaId":f.replica,"operationIdPrefix":format!("invalid-source-{number}")});
        let (status, rejected) =
            call(client, base, &path, Method::POST, token, csrf, Some(input)).await?;
        assert_eq!(status, 409, "{rejected}");
        assert_eq!(
            rejected.get("code"),
            Some(&json!("CATALOG_PACKAGE_CARD_METADATA_INVALID"))
        );
        let counts:(i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM content.cards WHERE workspace_id=$1),(SELECT count(*) FROM content.media_assets WHERE workspace_id=$1),(SELECT count(*) FROM sync.hot_changes WHERE workspace_id=$1 AND entity_type IN('card','media_asset')),(SELECT count(*) FROM sync.catalog_package_install_idempotency WHERE workspace_id=$1)").bind(f.workspace).fetch_one(owner).await?;
        assert_eq!(
            counts,
            (2, 1, 3, 1),
            "Invalid provenance left partial imported data"
        );
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "The real install flow checks durable identities, media rewrites, migration preservation, and conflicting retries together."
)]
async fn install_checks(
    client: &Client,
    base: &str,
    owner: &PgPool,
    f: &Fixture,
    token: &str,
    csrf: &str,
) -> Result<()> {
    let path = format!(
        "/v1/workspaces/{}/catalog/package-versions/{}/install",
        f.workspace, f.version
    );
    let input = json!({"installId":"\u{feff}catalog-test\u{feff}","installedAt":"2026-06-01T12:00:00.000Z","clientUpdatedAt":"2026-06-01T12:00:00.000Z","lastModifiedByReplicaId":f.replica,"operationIdPrefix":"catalog-fixture","addImportTag":true,"importTag":"import:fixture","removeTags":["remove","Ｚ","😀"]});
    let (status, result) = call(
        client,
        base,
        &path,
        Method::POST,
        token,
        csrf,
        Some(input.clone()),
    )
    .await?;
    assert_eq!(status, 200, "{result}");
    let (_, replay) = call(
        client,
        base,
        &path,
        Method::POST,
        token,
        csrf,
        Some(input.clone()),
    )
    .await?;
    assert_eq!(replay, result, "Retry minted new identities");
    let media = result
        .pointer("/installedMediaAssets/0/mediaAssetId")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("Media absent"))?;
    let cards:Vec<(String,Vec<String>,Value,chrono::DateTime<chrono::Utc>)>=sqlx::query_as("SELECT back_text,tags,metadata,created_at FROM content.cards WHERE workspace_id=$1 ORDER BY created_at").bind(f.workspace).fetch_all(owner).await?;
    assert_eq!(cards.len(), 2);
    let first = cards.first().ok_or_else(|| eyre!("Card absent"))?;
    assert!(
        first.0.contains(&format!("![diagram](fcasset:{media})")),
        "{}",
        first.0
    );
    assert!(
        first.0.contains(&format!("[figure]: fcasset:{media}")),
        "{}",
        first.0
    );
    assert!(first.0.contains(&format!(
        "![titled](fcasset:{media} \"caption ]( literal\")"
    )));
    assert!(first.0.contains(&format!("![escaped](fcasset:{media})")));
    assert!(first.0.contains(&format!("![entity](fcasset:{media})")));
    assert!(
        first.0.contains("`![code](fcasset:missing)`")
            && first.0.contains("```md\n![code](fcasset:missing)\n```")
    );
    assert_eq!(first.1, vec!["keep", "import:fixture"]);
    assert_eq!(
        first.2.pointer("/source/label"),
        Some(&json!("Fixture title"))
    );
    assert_eq!(
        cards
            .last()
            .ok_or_else(|| eyre!("Card absent"))?
            .3
            .signed_duration_since(first.3)
            .num_milliseconds(),
        1
    );
    let hot_count:i64=sqlx::query_scalar("SELECT count(*) FROM sync.hot_changes WHERE workspace_id=$1 AND entity_type IN('card','media_asset')").bind(f.workspace).fetch_one(owner).await?;
    assert_eq!(hot_count, 3);
    migrations::verify(owner, f.workspace).await?;
    sqlx::query("UPDATE sync.catalog_package_install_idempotency SET install_result=install_result #- '{summary,keptTagCount}' WHERE workspace_id=$1 AND install_id='catalog-test'").bind(f.workspace).execute(owner).await?;
    let (status, invalid) = call(
        client,
        base,
        &path,
        Method::POST,
        token,
        csrf,
        Some(input.clone()),
    )
    .await?;
    assert_eq!(status, 500, "{invalid}");
    assert_eq!(
        invalid.get("code"),
        Some(&json!("CATALOG_PACKAGE_INSTALL_STORED_RESULT_INVALID"))
    );
    sqlx::query("UPDATE sync.catalog_package_install_idempotency SET install_result=$2 WHERE workspace_id=$1 AND install_id='catalog-test'").bind(f.workspace).bind(&result).execute(owner).await?;
    let mut changed = input.clone();
    set(
        &mut changed,
        "clientUpdatedAt",
        json!("2026-06-01T12:00:00.001Z"),
    )?;
    let (status, conflict) = call(
        client,
        base,
        &path,
        Method::POST,
        token,
        csrf,
        Some(changed),
    )
    .await?;
    assert_eq!(status, 409, "{conflict}");
    assert_eq!(
        conflict.get("code"),
        Some(&json!("CATALOG_PACKAGE_INSTALL_IDEMPOTENCY_CONFLICT"))
    );
    let mut changed = input;
    set(&mut changed, "installId", json!("different-install"))?;
    let (status, conflict) = call(
        client,
        base,
        &path,
        Method::POST,
        token,
        csrf,
        Some(changed),
    )
    .await?;
    assert_eq!(status, 409, "{conflict}");
    assert_eq!(
        conflict.get("code"),
        Some(&json!("CATALOG_PACKAGE_INSTALL_OPERATION_ALREADY_EXISTS"))
    );
    Ok(())
}

#[allow(
    clippy::indexing_slicing,
    reason = "The fixture asserts known JSON shapes from real HTTP responses."
)]
pub async fn verify(
    client: &Client,
    base: &str,
    owner: &PgPool,
    user: Uuid,
    token: &str,
    csrf: &str,
) -> Result<()> {
    let f = fixture(client, base, owner, user, token, csrf).await?;
    let result = std::panic::AssertUnwindSafe(async {
        public_reads(client, base, &f, token, csrf).await?;
        install_checks(client, base, owner, &f, token, csrf).await?;
        private_markdown(client, base, owner, &f).await?;
        invalid_provenance(client, base, owner, &f, token, csrf).await
    })
    .catch_unwind()
    .await;
    sqlx::query("DELETE FROM org.workspaces WHERE workspace_id=$1")
        .bind(f.workspace)
        .execute(owner)
        .await?;
    sqlx::query("DELETE FROM catalog.packages WHERE package_id=$1")
        .bind(f.package)
        .execute(owner)
        .await?;
    sqlx::query("DELETE FROM catalog.authors WHERE author_id=$1")
        .bind(f.author)
        .execute(owner)
        .await?;
    sqlx::query("DELETE FROM content.media_blobs WHERE media_blob_id=$1")
        .bind(f.blob)
        .execute(owner)
        .await?;
    result.map_err(|_| eyre!("Catalog HTTP fixture assertion failed"))?
}
