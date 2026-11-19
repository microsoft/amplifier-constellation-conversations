//! Presentation data belongs to the saved agent endpoint. Reading a cached
//! portrait never starts an endpoint or grants access to a conversation.
use crate::{
    auth, driver,
    error::{ApiError, ApiResult},
    state::AppState,
    update_admission,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::Cursor, sync::Arc};

const MAX_BYTES: usize = 1024 * 1024;

pub(crate) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS agent_portraits(
        agent_id TEXT PRIMARY KEY REFERENCES agent_endpoints(id) ON DELETE CASCADE,
        digest TEXT NOT NULL, png BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS agent_portrait_bindings(
        agent_id TEXT PRIMARY KEY REFERENCES agent_endpoints(id) ON DELETE CASCADE,
        generation BLOB NOT NULL);
        CREATE TRIGGER IF NOT EXISTS rebind_portrait_observer AFTER UPDATE OF config ON agent_endpoints
        BEGIN UPDATE agent_portrait_bindings SET generation=randomblob(16) WHERE agent_id=new.id; END;
        CREATE TRIGGER IF NOT EXISTS invalidate_agent_portrait AFTER UPDATE OF config ON agent_endpoints
        WHEN old.config != new.config BEGIN DELETE FROM agent_portraits WHERE agent_id=old.id; END;")?;
    Ok(())
}

// Decode to pixels and encode a fresh static PNG. The peer cannot supply an
// external image URL, scripts, text chunks or other metadata to the browser.
fn pixels(doc: &Value) -> Result<Option<Vec<u8>>> {
    ensure!(
        doc["schema"] == "agent-identity/1"
            && doc["agent_id"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && s.len() <= 256),
        "Invalid agent identity"
    );
    let avatar = &doc["avatar"];
    ensure!(
        avatar["media_type"] == "image/png"
            && matches!(avatar["kind"].as_str(), Some("uploaded" | "generated")),
        "Unsupported portrait"
    );
    let data = avatar["data"].as_str().context("Missing image")?;
    ensure!(data.len() <= MAX_BYTES.div_ceil(3) * 4, "Image too large");
    let bytes = STANDARD.decode(data)?;
    ensure!(
        bytes.len() <= MAX_BYTES
            && avatar["bytes"].as_u64() == Some(bytes.len() as u64)
            && avatar["sha256"] == format!("{:x}", Sha256::digest(&bytes)),
        "Portrait checksum or size mismatch"
    );
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: 16 * MAX_BYTES,
        },
    );
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let info = reader.info();
    let size = info.width;
    ensure!(
        (512..=1024).contains(&size)
            && size == info.height
            && info.animation_control.is_none()
            && avatar["width"].as_u64() == Some(size.into())
            && avatar["height"].as_u64() == Some(size.into()),
        "Invalid portrait dimensions"
    );
    let mut buffer = vec![
        0;
        reader
            .output_buffer_size()
            .filter(|n| *n <= 4 * MAX_BYTES)
            .context("Invalid pixel size")?
    ];
    let frame = reader.next_frame(&mut buffer)?;
    reader.finish()?;
    if avatar["kind"] == "generated" {
        return Ok(None);
    }
    let mut result = Vec::new();
    {
        let mut output = png::Encoder::new(&mut result, size, size);
        output.set_color(frame.color_type);
        output.set_depth(frame.bit_depth);
        let mut writer = output.write_header()?;
        writer.write_image_data(&buffer[..frame.buffer_size()])?;
        writer.finish()?;
    }
    Ok(Some(result))
}

pub(crate) fn digest(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT digest FROM agent_portraits WHERE agent_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?)
}

pub(crate) async fn get(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    auth::check(&s, &h, "observe", false)?;
    let image: Vec<u8> =
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT png FROM agent_portraits WHERE agent_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(ApiError::missing)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "private, no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        image,
    )
        .into_response())
}

pub(crate) async fn refresh(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    let permit = s
        .portrait_refresh
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::bad("A contact image is already being refreshed."))?;
    let activity = update_admission::begin_probe(&s)?;
    let (agent, binding) = {
        let mut db = s.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO agent_portrait_bindings(agent_id,generation)
             SELECT id,randomblob(16) FROM agent_endpoints WHERE id=?1",
            [&id],
        )?;
        let (config, binding): (String, Vec<u8>) = tx
            .query_row(
                "SELECT e.config,b.generation FROM agent_endpoints e
             JOIN agent_portrait_bindings b ON b.agent_id=e.id WHERE e.id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(ApiError::missing)?;
        let agent = serde_json::from_str(&config).map_err(anyhow::Error::from)?;
        tx.commit()?;
        (agent, binding)
    };
    // The child and its admission guard outlive a disconnected HTTP client.
    let observed = async move {
        let (_permit, _activity) = (permit, activity);
        let doc = driver::observe_identity(&agent)
            .await
            .map_err(|_| ApiError::bad("The agent could not provide its contact image."))?;
        let image = doc
            .as_ref()
            .map(pixels)
            .transpose()
            .map_err(|_| ApiError::bad("The agent supplied an invalid contact image."))?
            .flatten();
        auth::check(&s, &h, "admin", true)?;
        let mut db = s.db.lock().unwrap();
        let tx = db.transaction()?;
        let still_bound: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_portrait_bindings WHERE agent_id=?1 AND generation=?2)",
            params![id,binding], |r| r.get(0),
        )?;
        if !still_bound {
            return Err(ApiError::bad(
                "This agent connection changed during refresh.",
            ));
        }
        let digest = if let Some(png) = image {
            let digest = format!("{:x}", Sha256::digest(&png));
            tx.execute(
                "INSERT INTO agent_portraits(agent_id,digest,png) VALUES(?1,?2,?3)
                 ON CONFLICT(agent_id) DO UPDATE SET digest=excluded.digest,png=excluded.png",
                params![id, digest, png],
            )?;
            Some(digest)
        } else {
            tx.execute("DELETE FROM agent_portraits WHERE agent_id=?1", [&id])?;
            None
        };
        tx.commit()?;
        Ok(Json(
            json!({"avatar_digest":digest,"supported":doc.is_some()}),
        ))
    };
    tokio::spawn(observed).await.map_err(anyhow::Error::from)?
}

/// Freeze the exact selected agent's picture in the requesting conversation.
/// A later contact edit or image removal does not change a historical card.
pub(crate) fn share(
    tx: &rusqlite::Transaction<'_>,
    agent: &str,
    room: &str,
    title: &str,
) -> ApiResult<Value> {
    let image: Vec<u8> = tx
        .query_row(
            "SELECT png FROM agent_portraits WHERE agent_id=?1",
            [agent],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| ApiError::bad("This contact has no saved portrait."))?;
    let used: i64 = tx.query_row(
        "SELECT coalesce(sum(bytes),0) FROM attachments WHERE conversation_id=?1",
        [room],
        |r| r.get(0),
    )?;
    if used + image.len() as i64 > 64 * 1024 * 1024 {
        return Err(ApiError::bad("Conversation attachment limit reached"));
    }
    let now = crate::state::now_ms();
    let message = uuid::Uuid::new_v4().to_string();
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES(?1,?2,'assistant',?3,'complete',?4,?4,?5,?5)",params![message,room,title,now,agent])?;
    tx.execute("INSERT INTO attachments(id,conversation_id,name,mime_type,data,bytes,created_by,created_at,message_id) VALUES(?1,?2,'portrait.png','image/png',?3,?4,?5,?6,?7)",params![id,room,image,image.len() as i64,agent,now,message])?;
    Ok(json!({"id":id,"name":"portrait.png","mime_type":"image/png","bytes":image.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document() -> Value {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 512, 512);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .add_text_chunk("private".into(), "remove-this-metadata".into())
                .unwrap();
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&vec![94; 512 * 512])
                .unwrap();
        }
        json!({"schema":"agent-identity/1","agent_id":"installed","avatar":{"kind":"uploaded","media_type":"image/png","width":512,"height":512,"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes)),"data":STANDARD.encode(bytes)}})
    }
    #[test]
    fn uploaded_pixels_survive_but_metadata_does_not() {
        let d = document();
        let p = pixels(&d).unwrap().unwrap();
        assert!(!p.windows(20).any(|b| b == b"remove-this-metadata"));
        let mut reader = png::Decoder::new(Cursor::new(p)).read_info().unwrap();
        let mut data = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut data).unwrap();
        assert_eq!(data, vec![94; 512 * 512]);
        let mut generated = d;
        generated["avatar"]["kind"] = json!("generated");
        assert!(pixels(&generated).unwrap().is_none());
    }
    #[test]
    fn invalid_size_checksum_or_revision_body_does_not_replace_saved_pixels() {
        for (key, bad) in [
            ("width", json!(90000)),
            ("height", json!(12)),
            ("sha256", json!("wrong")),
            ("bytes", json!(11)),
            ("media_type", json!("image/svg+xml")),
            ("data", json!("https://example.invalid/picture")),
        ] {
            let mut d = document();
            d["avatar"][key] = bad;
            assert!(pixels(&d).is_err());
        }
    }
}
