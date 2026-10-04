use super::set;
use color_eyre::eyre::Result;
use reqwest::Client;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

pub const EVENT: &str = "0194da77-8100-7001-8111-000000000005";

pub async fn verify(client: &Client, base: &str, owner: &PgPool, token: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let body = json!({"eventId":EVENT,"eventName":"consent_prompt_shown","clientOccurredAt":now,"clientSentAt":now,"uiLocale":"iw-IL","properties":{}});
    let path = format!("{base}/v1/analytics/anonymous-events");
    let request = || {
        client
            .post(&path)
            .header("origin", "http://localhost:3000")
            .header(
                "user-agent",
                "Mozilla/5.0 (X11; Linux x86_64) Firefox/130.0",
            )
            .header("cookie", format!("session={token}"))
    };
    let response = request().json(&body).send().await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.json::<Value>().await?, json!({"accepted":true}));
    assert_eq!(request().json(&body).send().await?.status(), 200);
    let stored:Value=sqlx::query_scalar("SELECT jsonb_build_object('user',user_id,'subject',subject_user_id,'session',session_id,'anonymous',anonymous_id,'ip',daily_visitor_hash,'country',country,'platform',platform,'trust',trust_level,'locale',ui_locale,'automated',automated_client) FROM analytics.product_events WHERE event_id=$1").bind(EVENT.parse::<Uuid>()?).fetch_one(owner).await?;
    assert_eq!(
        stored,
        json!({"user":null,"subject":null,"session":null,"anonymous":null,"ip":null,"country":null,"platform":"web","trust":"anonymous_client","locale":"he-IL","automated":false})
    );
    let mut invalid = body.clone();
    set(&mut invalid, "anonymousId", json!(Uuid::new_v4()))?;
    assert_eq!(request().json(&invalid).send().await?.status(), 400);
    let mut invalid = body.clone();
    set(&mut invalid, "userId", json!(Uuid::new_v4()))?;
    assert_eq!(request().json(&invalid).send().await?.status(), 400);
    let mut invalid = body.clone();
    set(
        &mut invalid,
        "properties",
        json!({"card_text":"Private text"}),
    )?;
    assert_eq!(request().json(&invalid).send().await?.status(), 400);
    let mut invalid = body.clone();
    set(&mut invalid, "eventName", json!("catalog_deck_installed"))?;
    assert_eq!(request().json(&invalid).send().await?.status(), 400);
    assert_eq!(
        client
            .post(&path)
            .header("origin", "https://untrusted.invalid")
            .json(&body)
            .send()
            .await?
            .status(),
        403
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM analytics.product_events WHERE event_id=$1")
            .bind(EVENT.parse::<Uuid>()?)
            .fetch_one(owner)
            .await?;
    assert_eq!(count, 1);
    Ok(())
}
