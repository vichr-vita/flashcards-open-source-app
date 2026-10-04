//! Real HTTP and restricted-role `PostgreSQL` checks for portable package and community boundaries.

use axum::http::HeaderMap;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use color_eyre::eyre::{Result, ensure, eyre};
use lingvichr::{AppState, Config, ancillary, auth, core, database};
use reqwest::{
    Client, RequestBuilder,
    multipart::{Form, Part},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{
    future::IntoFuture,
    io::{Cursor, Read, Write},
    path::PathBuf,
    sync::Arc,
};
use uuid::Uuid;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

type FriendFact = (Uuid, DateTime<Utc>, DateTime<Utc>, Option<String>, Value);

fn zip(entries: &[(&str, Vec<u8>)], deflate: bool) -> Result<Vec<u8>> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes) in entries {
        writer.start_file(
            *path,
            SimpleFileOptions::default().compression_method(if deflate {
                CompressionMethod::Deflated
            } else {
                CompressionMethod::Stored
            }),
        )?;
        writer.write_all(bytes)?;
    }
    Ok(writer.finish()?.into_inner())
}
fn signed(request: RequestBuilder, token: &str, csrf: &str) -> RequestBuilder {
    request
        .header("cookie", format!("session={token}"))
        .header("origin", "http://localhost:3000")
        .header("x-csrf-token", csrf)
}
async fn response(request: RequestBuilder) -> Result<Value> {
    let response = request.send().await?;
    let status = response.status();
    let url = response.url().to_string();
    let value: Value = response.json().await?;
    ensure!(status.is_success(), "HTTP {status} at {url}: {value}");
    Ok(value)
}
async fn import(
    client: &Client,
    url: &str,
    token: &str,
    csrf: &str,
    bytes: Vec<u8>,
    options: &Value,
) -> Result<reqwest::Response> {
    let form = Form::new()
        .part(
            "file",
            Part::bytes(bytes)
                .file_name("flashcards.zip")
                .mime_str("application/zip")?,
        )
        .text("options", options.to_string());
    Ok(signed(client.post(url), token, csrf)
        .multipart(form)
        .send()
        .await?)
}

async fn leaderboard_reads(
    client: &Client,
    base: &str,
    token: &str,
    csrf: &str,
    other_user: &str,
    other_profile: Uuid,
) -> Result<()> {
    let rating = response(signed(
        client.get(format!("{base}/v1/me/progress/leaderboard")),
        token,
        csrf,
    ))
    .await?;
    ensure!(
        rating.get("status") == Some(&json!("ready")),
        "Rating snapshots failed: {rating}"
    );
    let windows = rating
        .get("windows")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre!("Ranking windows absent"))?;
    ensure!(
        windows.len() == 5
            && windows
                .iter()
                .all(|window| window.pointer("/viewer/rank") == Some(&json!(2))
                    && window.get("participantCount") == Some(&json!(2))),
        "Rating viewer tie changed: {rating}"
    );
    ensure!(
        !rating.to_string().contains(other_user),
        "Ranking disclosed private account identity"
    );
    let streak_response = response(signed(
        client.get(format!("{base}/v1/me/progress/leaderboards/streak")),
        token,
        csrf,
    ))
    .await?;
    ensure!(
        streak_response.pointer("/viewer/rank") == Some(&json!(1)),
        "Streak viewer tie changed: {streak_response}"
    );
    let details = response(signed(
        client.get(format!(
            "{base}/v1/me/progress/leaderboards/profiles/{other_profile}"
        )),
        token,
        csrf,
    ))
    .await?;
    ensure!(
        details.get("status") == Some(&json!("ready"))
            && details
                .pointer("/reviewActivity/days")
                .and_then(Value::as_array)
                .is_some_and(|days| days.len() == 30),
        "Stored public profile contract failed: {details}"
    );
    ensure!(
        !details.to_string().contains(other_user),
        "Profile disclosed private account identity"
    );
    Ok::<(), color_eyre::eyre::Report>(())
}

async fn leaderboard_contract(
    owner: &PgPool,
    client: &Client,
    base: &str,
    token: &str,
    csrf: &str,
    profile: &Value,
) -> Result<()> {
    let viewer = profile
        .get("publicProfileId")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("Profile identity missing"))?
        .parse::<Uuid>()?;
    let other_user = Uuid::new_v4().to_string();
    let other_profile = Uuid::new_v4();
    sqlx::query("INSERT INTO org.user_settings(user_id) VALUES($1)")
        .bind(&other_user)
        .execute(owner)
        .await?;
    sqlx::query("INSERT INTO community.public_profiles(user_id,public_profile_id) VALUES($1,$2)")
        .bind(&other_user)
        .bind(other_profile)
        .execute(owner)
        .await?;
    let mut snapshots = Vec::new();
    for window in [
        "last_24_hours",
        "last_3_days",
        "last_7_days",
        "last_30_days",
        "all_time",
    ] {
        let snapshot = Uuid::new_v4();
        sqlx::query("INSERT INTO community.leaderboard_snapshots(snapshot_id,metric_version,window_key,generated_at,as_of_server_hour) VALUES($1,'qualified_reviews_v1',$2,now(),now()+interval '10 years')")
            .bind(snapshot).bind(window).execute(owner).await?;
        sqlx::query("INSERT INTO community.leaderboard_snapshot_entries(snapshot_id,public_profile_id,qualified_review_count,base_sort_position) VALUES($1,$2,5,1),($1,$3,5,2)")
            .bind(snapshot).bind(other_profile).bind(viewer).execute(owner).await?;
        snapshots.push(snapshot);
    }
    let streak = Uuid::new_v4();
    sqlx::query("INSERT INTO community.streak_leaderboard_snapshots(snapshot_id,metric_version,as_of_utc_date,generated_at) VALUES($1,'streak_days_v1',current_date+3650,now())").bind(streak).execute(owner).await?;
    sqlx::query("INSERT INTO community.streak_leaderboard_snapshot_entries(snapshot_id,public_profile_id,streak_days,base_sort_position) VALUES($1,$2,5,1),($1,$3,5,2)")
        .bind(streak).bind(other_profile).bind(viewer).execute(owner).await?;
    let validation = leaderboard_reads(client, base, token, csrf, &other_user, other_profile).await;
    sqlx::query("DELETE FROM community.leaderboard_snapshots WHERE snapshot_id=ANY($1)")
        .bind(snapshots)
        .execute(owner)
        .await?;
    sqlx::query("DELETE FROM community.streak_leaderboard_snapshots WHERE snapshot_id=$1")
        .bind(streak)
        .execute(owner)
        .await?;
    sqlx::query("DELETE FROM org.user_settings WHERE user_id=$1")
        .bind(other_user)
        .execute(owner)
        .await?;
    validation
}

async fn friend_acceptance_facts(
    owner: &PgPool,
    client: &Client,
    base: &str,
    token: &str,
    csrf: &str,
) -> Result<()> {
    let inviter = Uuid::new_v4();
    let inviter_text = inviter.to_string();
    let invitation = Uuid::new_v4();
    let invitation_token = Uuid::new_v4().simple().to_string();
    sqlx::query("INSERT INTO org.user_settings(user_id) VALUES($1)")
        .bind(&inviter_text)
        .execute(owner)
        .await?;
    sqlx::query("INSERT INTO community.public_profiles(user_id,public_profile_id) VALUES($1,$2)")
        .bind(&inviter_text)
        .bind(Uuid::new_v4())
        .execute(owner)
        .await?;
    sqlx::query("INSERT INTO community.friend_invitations(friend_invitation_id,inviter_user_id,invite_token_hash,invitee_display_name_for_inviter,expires_at) VALUES($1,$2,$3,'Private friend',now()+interval '2 days')")
        .bind(invitation).bind(&inviter_text).bind(format!("{:x}",Sha256::digest(invitation_token.as_bytes()))).execute(owner).await?;
    let validation = async {
        let account: String = sqlx::query_scalar("SELECT user_id FROM auth.local_account").fetch_one(owner).await?;
        let account = account.parse::<Uuid>()?;
        let viewers = vec![account,inviter];
        let before = Utc::now();
        let url = format!("{base}/v1/me/community/friend-invitations/{invitation_token}/accept");
        let accepted = response(signed(client.post(&url),token,csrf).json(&json!({"inviterDisplayName":"A friend"}))).await?;
        ensure!(accepted.get("status")==Some(&json!("accepted")),"Stored invitation acceptance failed: {accepted}");
        let rows: Vec<FriendFact> = sqlx::query_as("SELECT user_id,occurred_at,server_received_at,platform,event_properties FROM analytics.product_events WHERE event_name='friendship_created' AND user_id=ANY($1) AND occurred_at>=$2")
            .bind(&viewers).bind(before).fetch_all(owner).await?;
        ensure!(rows.len()==2 && viewers.iter().all(|viewer|rows.iter().any(|row|row.0==*viewer)) && rows.iter().all(|row|row.1==row.2 && row.3.is_none() && row.4==json!({})),"Directed friendship facts lost identities, database time or platform privacy");
        let replay = response(signed(client.post(&url),token,csrf).json(&json!({"inviterDisplayName":"A friend"}))).await?;
        ensure!(replay.get("status")==Some(&json!("inactive")),"Accepted invitation could be reused: {replay}");
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM analytics.product_events WHERE event_name='friendship_created' AND user_id=ANY($1) AND occurred_at>=$2").bind(viewers).bind(before).fetch_one(owner).await?;
        ensure!(count==2,"Invitation replay duplicated directed facts");
        Ok::<(),color_eyre::eyre::Report>(())
    }.await;
    sqlx::query("DELETE FROM analytics.product_events WHERE user_id=$1")
        .bind(inviter)
        .execute(owner)
        .await?;
    sqlx::query("DELETE FROM org.user_settings WHERE user_id=$1")
        .bind(inviter_text)
        .execute(owner)
        .await?;
    validation
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One real account/workspace fixture exercises package storage and sync together, then cleans up once."
)]
async fn portable_package_round_trip_and_safety() -> Result<()> {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let Ok(owner_url) = std::env::var("CORE_TEST_DATABASE_URL") else {
        return Ok(());
    };
    let owner = PgPool::connect(&owner_url).await?;
    let existing: Option<String> = sqlx::query_scalar("SELECT user_id FROM auth.local_account")
        .fetch_optional(&owner)
        .await?;
    let created = existing.is_none();
    let user = existing.unwrap_or_else(|| Uuid::new_v4().to_string());
    if created {
        sqlx::query(
            "INSERT INTO org.user_settings(user_id,progress_time_zone) VALUES($1,'Europe/Prague')",
        )
        .bind(&user)
        .execute(&owner)
        .await?;
        sqlx::query("INSERT INTO auth.local_account(user_id,webauthn_user_handle) VALUES($1,$1)")
            .bind(&user)
            .execute(&owner)
            .await?;
    }
    let state = AppState {
        pool: PgPool::connect(&std::env::var("CORE_TEST_BACKEND_URL")?).await?,
        auth_pool: PgPool::connect(&std::env::var("CORE_TEST_AUTH_URL")?).await?,
        config: Arc::new(Config {
            backend_origin: "http://localhost:3000".into(),
            auth_origin: "http://localhost:3000".into(),
            rp_id: "localhost".into(),
            cookie_domain: "localhost".into(),
            allowed_origins: vec!["http://localhost:3000".into()],
            csrf_secret: "disposable-test-secret-with-at-least-32-bytes".into(),
            allow_http: true,
            chatgpt_connection_dir: None,
            web_dir: PathBuf::new(),
            local_mcp_user_id: None,
        }),
    };
    let mut token_bytes = Vec::new();
    token_bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    token_bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    let token = URL_SAFE_NO_PAD.encode(token_bytes);
    let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
    sqlx::query("INSERT INTO auth.local_sessions(session_hash,refresh_hash,user_id,expires_at,refresh_expires_at) VALUES($1,$2,$3,now()+interval '1 hour',now()+interval '2 hours')").bind(&hash).bind(format!("{:x}",Sha256::digest(Uuid::new_v4().as_bytes()))).bind(&user).execute(&owner).await?;
    let mut headers = HeaderMap::new();
    headers.insert("cookie", format!("session={token}").parse()?);
    let identity = auth::authenticate(&state, &headers)
        .await
        .map_err(|error| eyre!("{}", error.message))?;
    let csrf = identity.csrf_token;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(
        axum::serve(
            listener,
            core::router()
                .merge(ancillary::router())
                .with_state(state.clone()),
        )
        .into_future(),
    );
    let client = Client::new();
    let workspace = response(
        signed(client.post(format!("{base}/v1/workspaces")), &token, &csrf)
            .json(&json!({"name":"Package integration"})),
    )
    .await?;
    let workspace = workspace
        .pointer("/workspace/workspaceId")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("workspace absent"))?
        .parse::<Uuid>()?;
    let validation=async {
        let mut tx=database::scoped(&state.pool,&user,Some(&workspace.to_string())).await.map_err(|error|eyre!("{}",error.message))?;
        let replica=core::ensure_system_replica(&mut tx,&user,workspace,"workspace_seed","workspace-seed").await.map_err(|error|eyre!("{}",error.message))?;tx.commit().await?;
        let package=json!({"formatVersion":1,"label":"Test source","author":"Source author","createdAt":"2026-02-03T00:00:00.000Z","sourceUrl":"https://example.invalid/source","cards":[{"frontText":"  Question  ","backText":" Answer ","tags":["kept","removed","kept"],"cardType":"basic","metadata":{"version":1,"source":{"label":"Card source"}}},{"frontText":"Code example","backText":"```md\n![example](media/code.png)\n```","tags":["kept"],"cardType":"basic","metadata":{"version":1,"source":null}}]});
        let bytes=zip(&[("cards.json",serde_json::to_vec(&package)?)],true)?;
        let import_url=format!("{base}/v1/workspaces/{workspace}/packages/import");
        let preview_url=format!("{import_url}/preview");
        let preview=response(signed(client.post(&preview_url),&token,&csrf).header("content-type","application/zip").body(bytes.clone())).await?;
        ensure!(preview.get("cardCount")==Some(&json!(2))&&preview.get("referencedMediaCount")==Some(&json!(0)),"preview contract: {preview}");
        ensure!(preview.pointer("/defaultOptions/addImportTag")==Some(&json!(true)),"import default absent");
        ensure!(preview.pointer("/tagCounts/0")==Some(&json!({"tag":"kept","cardsCount":3})),"source tag occurrences were deduplicated before preview");
        ensure!(preview.pointer("/defaultOptions/suggestedImportTag")==Some(&json!(format!("import:{}-0",Utc::now().format("%Y-%m-%d")))),"deployed import-tag suggestion changed");
        let options=json!({"addImportTag":true,"importTag":"import:test","removeTags":["removed"],"importedAt":"2026-04-01T12:00:00.000Z","importId":"test-import","clientUpdatedAt":Utc::now(),"lastModifiedByReplicaId":replica,"operationIdPrefix":"integration"});
        let imported=import(&client,&import_url,&token,&csrf,bytes.clone(),&options).await?;let status=imported.status();let imported:Value=imported.json().await?;
        ensure!(status.is_success(),"import failed {status}: {imported}");
        ensure!(imported.pointer("/summary/cardCount")==Some(&json!(2)),"import summary {imported}");
        ensure!(imported.pointer("/cards/0/frontText")==Some(&json!("Question")),"front text normalization lost");
        ensure!(imported.pointer("/cards/0/tags")==Some(&json!(["kept","import:test"])),"tag policy lost");
        ensure!(imported.pointer("/cards/0/metadata/source/label")==Some(&json!("Card source")),"card source precedence lost");
        ensure!(imported.pointer("/cards/0/metadata/source/author")==Some(&json!("Source author")),"package source fallback lost");
        ensure!(imported.pointer("/cards/1/metadata/source/importedAt")==options.get("importedAt"),"source provenance lost");
        let stored:i64=sqlx::query_scalar("SELECT count(*) FROM content.cards WHERE workspace_id=$1").bind(workspace).fetch_one(&owner).await?;
        let changes:i64=sqlx::query_scalar("SELECT count(*) FROM sync.hot_changes WHERE workspace_id=$1 AND entity_type='card'").bind(workspace).fetch_one(&owner).await?;
        ensure!(stored==2&&changes==2,"import did not atomically enter sync");
        let creation_facts:i64=sqlx::query_scalar(r#"SELECT count(*) FROM analytics.product_events WHERE user_id=$1::uuid AND workspace_id=$2 AND event_name='card_created' AND event_properties='{"source":"package_import"}'::jsonb AND subject_user_id=$1::uuid AND platform IS NULL"#).bind(&user).bind(workspace).fetch_one(&owner).await?;
        ensure!(creation_facts==2,"package card facts lost their creation source or trusted attribution");
        let package_facts:i64=sqlx::query_scalar(r#"SELECT count(*) FROM analytics.product_events WHERE user_id=$1::uuid AND workspace_id=$2 AND event_name='workspace_package_imported' AND event_properties='{"card_count":2}'::jsonb AND subject_user_id=$1::uuid AND platform IS NULL"#).bind(&user).bind(workspace).fetch_one(&owner).await?;
        ensure!(package_facts==1,"committed package import fact absent");
        let export_input=json!({"selection":{"kind":"allActiveCards"},"tagPolicy":{"additionalRemovedTags":[]},"packageMetadata":{"label":null,"author":null,"comment":null,"createdAt":null,"sourceUrl":null}});
        let export_url=format!("{base}/v1/workspaces/{workspace}/packages/export");
        let preview=response(signed(client.post(format!("{export_url}/preview")),&token,&csrf).json(&export_input)).await?;
        ensure!(preview.get("selectedCardCount")==Some(&json!(2)),"export preview failed {preview}");
        let exported=signed(client.post(&export_url),&token,&csrf).json(&export_input).send().await?;ensure!(exported.status().is_success(),"export failed {}",exported.status());
        let export_facts:i64=sqlx::query_scalar("SELECT count(*) FROM analytics.product_events WHERE user_id=$1::uuid AND workspace_id=$2 AND event_name='workspace_package_exported' AND event_properties='{}'::jsonb AND subject_user_id=$1::uuid AND platform IS NULL").bind(&user).bind(workspace).fetch_one(&owner).await?;
        ensure!(export_facts==1,"successful ZIP export fact absent");
        let bytes=exported.bytes().await?;let mut archive=ZipArchive::new(Cursor::new(bytes))?;
        ensure!(archive.len()==1,"card-only export contains other entries");let mut text=String::new();archive.by_name("cards.json")?.read_to_string(&mut text)?;
        let exported:Value=serde_json::from_str(&text)?;
        ensure!(exported.get("formatVersion")==Some(&json!(1)),"export version changed");
        let cards=exported.get("cards").and_then(Value::as_array).ok_or_else(||eyre!("cards absent"))?;
        ensure!(cards.iter().all(|card|card.get("tags")==Some(&json!(["kept"]))),"import tag removal lost");
        let missing_replica=Uuid::new_v4();let mut invalid_options=options.clone();invalid_options.as_object_mut().ok_or_else(||eyre!("options fixture absent"))?.insert("lastModifiedByReplicaId".into(),json!(missing_replica));
        let rejected=import(&client,&import_url,&token,&csrf,zip(&[("cards.json",serde_json::to_vec(&package)?)],false)?,&invalid_options).await?;
        ensure!(rejected.status()==reqwest::StatusCode::BAD_REQUEST,"unregistered replica accepted");
        for entries in [vec![("cards.json",serde_json::to_vec(&package)?),("../escape",vec![1])],vec![("cards.json",serde_json::to_vec(&package)?),("media/a.png",vec![1]),("media/A.png",vec![2])]] {
            let rejected=signed(client.post(&preview_url),&token,&csrf).header("content-type","application/zip").body(zip(&entries,false)?).send().await?;
            ensure!(rejected.status()==reqwest::StatusCode::BAD_REQUEST,"unsafe ZIP accepted");
            let rejected:Value=rejected.json().await?;ensure!(rejected.get("code")==Some(&json!("WORKSPACE_PACKAGE_IMPORT_PREVIEW_ZIP_INVALID")),"unsafe ZIP contract changed: {rejected}");
        }
        let mut duplicate=zip(&[("cards.json",serde_json::to_vec(&package)?),("cargs.json",serde_json::to_vec(&package)?)],false)?;
        let positions:Vec<_>=duplicate.windows(9).enumerate().filter_map(|(index,bytes)|(bytes==b"cargs.json").then_some(index)).collect();
        for position in positions {let end=position.checked_add(9).ok_or_else(||eyre!("fixture offset overflow"))?;duplicate.get_mut(position..end).ok_or_else(||eyre!("fixture offset invalid"))?.copy_from_slice(b"cards.json");}
        let rejected=signed(client.post(&preview_url),&token,&csrf).header("content-type","application/zip").body(duplicate).send().await?;
        ensure!(rejected.status()==reqwest::StatusCode::BAD_REQUEST,"duplicate cards.json accepted");
        let bomb=zip(&[("cards.json",serde_json::to_vec(&package)?),("media/large.png",vec![0;16_777_217])],true)?;
        let rejected=signed(client.post(&preview_url),&token,&csrf).header("content-type","application/zip").body(bomb).send().await?;ensure!(rejected.status()==reqwest::StatusCode::PAYLOAD_TOO_LARGE,"decoded media limit ignored");
        let mut with_media=package.clone();*with_media.pointer_mut("/cards/0/backText").ok_or_else(||eyre!("fixture absent"))?=json!("![image](media/photo.png)");
        let media_zip=zip(&[("cards.json",serde_json::to_vec(&with_media)?),("media/photo.png",vec![137,80,78,71,13,10,26,10])],false)?;
        let rejected=import(&client,&import_url,&token,&csrf,media_zip,&options).await?;ensure!(rejected.status()==reqwest::StatusCode::INTERNAL_SERVER_ERROR,"unconfigured media storage reported success");
        let after:i64=sqlx::query_scalar("SELECT count(*) FROM content.cards WHERE workspace_id=$1").bind(workspace).fetch_one(&owner).await?;ensure!(stored==after,"failed media import partially created cards");
        let profile=response(signed(client.get(format!("{base}/v1/me/community/profile")),&token,&csrf)).await?;
        ensure!(profile.get("anonymousDisplayName").and_then(Value::as_str).is_some_and(|name|!name.is_empty()),"profile name missing {profile}");
        leaderboard_contract(&owner,&client,&base,&token,&csrf,&profile).await?;
        let invitation=response(signed(client.post(format!("{base}/v1/me/community/friend-invitations")),&token,&csrf).json(&json!({"inviteeDisplayName":"A friend"}))).await?;
        let invitation_facts:i64=sqlx::query_scalar("SELECT count(*) FROM analytics.product_events WHERE user_id=$1 AND event_name='friend_invitation_created'").bind(user.parse::<Uuid>()?).fetch_one(&owner).await?;
        ensure!(invitation_facts==1,"Committed friend invitation fact missing");
        let link=invitation.get("inviteUrl").and_then(Value::as_str).ok_or_else(||eyre!("invitation URL missing"))?;
        let invitation_token=link.rsplit('/').next().ok_or_else(||eyre!("invitation token missing"))?;
        let preview=response(client.get(format!("{base}/v1/community/friend-invitations/{invitation_token}"))).await?;ensure!(preview.get("status")==Some(&json!("active")),"public preview lost {preview}");
        let own=signed(client.post(format!("{base}/v1/me/community/friend-invitations/{invitation_token}/accept")),&token,&csrf).json(&json!({"inviterDisplayName":"Me"})).send().await?;ensure!(own.status()==reqwest::StatusCode::CONFLICT,"self invitation accepted");
        friend_acceptance_facts(&owner,&client,&base,&token,&csrf).await?;
        let mobile=response(signed(client.get(format!("{base}/v1/me/review-platform-summary")),&token,&csrf)).await?;ensure!(mobile.get("hasMobileReviewEvent").is_some_and(Value::is_boolean),"mobile contract lost");
        Ok::<(),color_eyre::eyre::Report>(())
    }.await;
    server.abort();
    sqlx::query("DELETE FROM org.workspaces WHERE workspace_id=$1")
        .bind(workspace)
        .execute(&owner)
        .await?;
    sqlx::query("DELETE FROM auth.local_sessions WHERE session_hash=$1")
        .bind(hash)
        .execute(&owner)
        .await?;
    if created {
        sqlx::query("DELETE FROM analytics.product_events WHERE user_id=$1")
            .bind(user.parse::<Uuid>()?)
            .execute(&owner)
            .await?;
        sqlx::query("DELETE FROM org.user_settings WHERE user_id=$1")
            .bind(user)
            .execute(&owner)
            .await?;
    }
    validation
}
