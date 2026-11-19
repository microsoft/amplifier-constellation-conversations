//! Editable text artifacts belong to their conversation. Each has one message,
//! a revision for concurrent edits, and receipts for retried operations.
use crate::{
    auth,
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub fn migrate(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS room_notes(
        id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
        title TEXT NOT NULL,body TEXT NOT NULL,revision INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS room_note_requests(
        conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        request_id TEXT NOT NULL,note_id TEXT NOT NULL REFERENCES room_notes(id) ON DELETE CASCADE,
        content_hash TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(conversation_id,request_id));")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub request_id: String,
    pub expected_revision: i64,
    pub title: String,
    pub text: String,
}
pub fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|v| v.hyphenated().to_string() == id)
}
pub fn valid_text(title: &str, text: &str) -> bool {
    !title.trim().is_empty()
        && title.len() <= 300
        && !title.contains(['\n', '\r'])
        && !text.trim().is_empty()
        && text.len() <= 1200
}
pub fn note(db: &Connection, room: &str, id: &str) -> rusqlite::Result<Option<Value>> {
    db.query_row("SELECT n.title,n.body,n.revision FROM room_notes n JOIN messages m ON m.id=n.id WHERE m.conversation_id=?1 AND n.id=?2",
        params![room,id],|r| Ok(json!({"note_id":id,"title":r.get::<_,String>(0)?,"text":r.get::<_,String>(1)?,"revision":r.get::<_,i64>(2)?}))).optional()
}
fn conflict() -> ApiError {
    ApiError(
        StatusCode::CONFLICT,
        "The note or request has changed. Read the current note before editing.".into(),
    )
}

pub fn put(tx: &Transaction, room: &str, id: &str, actor: &str, edit: &Edit) -> ApiResult<Value> {
    if !valid_id(id)
        || !valid_id(&edit.request_id)
        || edit.expected_revision < 0
        || !valid_text(&edit.title, &edit.text)
    {
        return Err(ApiError::bad(
            "Invalid note identity, revision, title or text",
        ));
    }
    let hash = format!(
        "{:x}",
        Sha256::digest(
            json!([id, actor, edit.expected_revision, edit.title, edit.text]).to_string()
        )
    );
    let prior:Option<(String,i64)>=tx.query_row("SELECT content_hash,revision FROM room_note_requests WHERE conversation_id=?1 AND request_id=?2",params![room,edit.request_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((saved, revision)) = prior {
        if saved != hash {
            return Err(conflict());
        }
        return Ok(json!({"state":"saved","note_id":id,"revision":revision,"replayed":true}));
    }
    crate::update_admission::ensure_open(tx)?;
    crate::huddles::check_open(tx, room)?;
    let writable: bool = tx
        .query_row(
            "SELECT read_only=0 FROM conversations WHERE id=?1",
            [room],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(ApiError::missing)?;
    if !writable {
        return Err(ApiError::bad(
            "Edit this conversation in its original experience",
        ));
    }
    let at = now_ms();
    let display = format!("{}\n\n{}", edit.title, edit.text);
    if edit.expected_revision == 0 {
        // Never adopt another message or another room's note for a new request.
        let occupied: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1)",
            [id],
            |r| r.get(0),
        )?;
        if occupied {
            return Err(conflict());
        }
        tx.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'artifact',?3,'room_note',?4,?4,?5)",params![id,room,display,at,actor])?;
        tx.execute(
            "INSERT INTO room_notes(id,title,body,revision) VALUES(?1,?2,?3,1)",
            params![id, edit.title, edit.text],
        )?;
    } else {
        let current = note(tx, room, id)?.ok_or_else(ApiError::missing)?;
        if current["revision"] != edit.expected_revision {
            return Err(conflict());
        }
        tx.execute(
            "UPDATE room_notes SET title=?2,body=?3,revision=revision+1 WHERE id=?1",
            params![id, edit.title, edit.text],
        )?;
        tx.execute(
            "UPDATE messages SET text=?2,updated_at=?3 WHERE id=?1",
            params![id, display, at],
        )?;
    }
    let revision = edit.expected_revision + 1;
    tx.execute(
        "INSERT INTO room_note_requests VALUES(?1,?2,?3,?4,?5)",
        params![room, edit.request_id, id, hash, revision],
    )?;
    Ok(json!({"state":"saved","note_id":id,"revision":revision,"replayed":false}))
}

pub async fn read(
    State(s): State<Arc<AppState>>,
    Path((room, id)): Path<(String, String)>,
    h: HeaderMap,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "observe", false)?;
    let db = s.db.lock().unwrap();
    member(&db, &room, &p.actor)?;
    Ok(Json(note(&db, &room, &id)?.ok_or_else(ApiError::missing)?))
}
fn member(db: &Connection, room: &str, actor: &str) -> ApiResult<()> {
    if !db.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_members WHERE conversation_id=?1 AND username=?2)",
        params![room, actor],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(ApiError::missing());
    }
    Ok(())
}
pub async fn edit(
    State(s): State<Arc<AppState>>,
    Path((room, id)): Path<(String, String)>,
    h: HeaderMap,
    Json(edit): Json<Edit>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    member(&tx, &room, &p.actor)?;
    let call: Option<String> = tx
        .query_row(
            "SELECT id FROM companion_calls WHERE conversation_id=?1",
            [&room],
            |r| r.get(0),
        )
        .optional()?;
    // The human can continue editing after hang-up or in an ordinary text room.
    // A saved call room still requires its original private audience and endpoint.
    if let Some(call) = call
        && crate::companion_history::private_identity(&tx, &call)?.is_none()
    {
        return Err(ApiError::forbidden());
    }
    let result = put(&tx, &room, &id, &p.actor, &edit)?;
    if result["replayed"] != true {
        crate::companion_tools::notify_note_edit(&tx, &room, &id)?;
    }
    tx.commit()?;
    drop(db);
    s.changed();
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion_tools::{self, Show};
    use crate::huddle_delivery::tests::fixture;
    use axum::http::header;
    use uuid::Uuid;

    fn bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    #[tokio::test]
    async fn display_requires_the_original_active_private_audience_for_every_kind() {
        let f = fixture();
        std::fs::write(
            f.s.home.join("companion-actions.json"),
            r#"{"enabled":true}"#,
        )
        .unwrap();
        let grant = bearer("saved-grant");
        f.s.db.lock().unwrap().execute("INSERT INTO companion_tool_grants(call_id,token,port) VALUES(?1,'saved-grant',12345)",[&f.call]).unwrap();
        let show = |body: Value| {
            companion_tools::show(
                State(f.s.clone()),
                Path(f.call.clone()),
                grant.clone(),
                Json(serde_json::from_value::<Show>(body).unwrap()),
            )
        };
        assert_eq!(
            show(json!({"kind":"note","title":"Open","text":"Private"}))
                .await
                .unwrap()
                .0["state"],
            "saved"
        );
        // A room that has ever been shared stays ineligible even when only the
        // original viewer and endpoint remain. The still-valid bearer is exact.
        f.s.db
            .lock()
            .unwrap()
            .execute("UPDATE conversations SET shared=1 WHERE id=?1", [&f.parent])
            .unwrap();
        let cases = [
            json!({"title":"Artifact","url":"https://example.test/demo"}),
            json!({"kind":"markdown","title":"Diagram","text":"Diagram source"}),
            json!({"kind":"portrait","title":"Portrait"}),
            json!({"kind":"note","title":"Private","text":"Saved","note_id":Uuid::new_v4().to_string(),"request_id":Uuid::new_v4().to_string(),"expected_revision":0}),
        ];
        for body in &cases {
            assert_eq!(
                show(body.clone()).await.unwrap_err().0,
                StatusCode::FORBIDDEN
            );
        }
        {
            let db = f.s.db.lock().unwrap();
            db.execute("UPDATE conversations SET shared=0 WHERE id=?1", [&f.parent])
                .unwrap();
        }

        // The same selected agent ID cannot authorize a replacement endpoint.
        let original_config: String = {
            let db = f.s.db.lock().unwrap();
            let raw = db
                .query_row(
                    "SELECT config FROM agent_endpoints WHERE id='default'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            db.execute(
                "UPDATE agent_endpoints SET config=json_set(config,'$.command','replacement-agent') WHERE id='default'",
                [],
            ).unwrap();
            raw
        };
        assert_eq!(
            show(cases[1].clone()).await.unwrap_err().0,
            StatusCode::FORBIDDEN
        );
        {
            let db = f.s.db.lock().unwrap();
            db.execute(
                "UPDATE agent_endpoints SET config=?1 WHERE id='default'",
                [&original_config],
            )
            .unwrap();
            db.execute(
                "UPDATE companion_tool_grants SET token='replacement-grant' WHERE call_id=?1",
                [&f.call],
            )
            .unwrap();
        }
        assert_eq!(
            show(cases[0].clone()).await.unwrap_err().0,
            StatusCode::FORBIDDEN
        );
        {
            let db = f.s.db.lock().unwrap();
            db.execute(
                "UPDATE companion_tool_grants SET token='saved-grant' WHERE call_id=?1",
                [&f.call],
            )
            .unwrap();
            db.execute(
                "UPDATE companion_calls SET state='closed' WHERE id=?1",
                [&f.call],
            )
            .unwrap();
        }
        for body in &cases {
            assert_eq!(
                show(body.clone()).await.unwrap_err().0,
                StatusCode::NOT_FOUND
            );
        }
        let db = f.s.db.lock().unwrap();
        let counts:(i64,i64,i64)=db.query_row("SELECT (SELECT COUNT(*) FROM companion_events WHERE call_id=?1 AND kind='card'),(SELECT COUNT(*) FROM room_notes),(SELECT COUNT(*) FROM attachments WHERE conversation_id=?2)",params![f.call,f.parent],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(counts, (1, 0, 0));
    }

    #[tokio::test]
    async fn human_note_edits_survive_hangup_and_refuse_private_audience_drift() {
        let f = fixture();
        let token = std::fs::read_to_string(f.s.home.join("secrets/control-token")).unwrap();
        let h = bearer(token.trim());
        let save = |room: &str, id: &str, revision: i64| {
            edit(
                State(f.s.clone()),
                Path((room.to_owned(), id.to_owned())),
                h.clone(),
                Json(Edit {
                    request_id: Uuid::new_v4().to_string(),
                    expected_revision: revision,
                    title: "Private plan".into(),
                    text: "Review notes".into(),
                }),
            )
        };
        let ordinary = f.s.create("Ordinary room").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let id = Uuid::new_v4().to_string();
        assert_eq!(
            save(&ordinary, &Uuid::new_v4().to_string(), 0)
                .await
                .unwrap()
                .0["revision"],
            1
        );
        assert_eq!(save(&f.parent, &id, 0).await.unwrap().0["revision"], 1);
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_calls SET state='closed' WHERE id=?1",
                [&f.call],
            )
            .unwrap();
        assert_eq!(save(&f.parent, &id, 1).await.unwrap().0["revision"], 2);
        f.s.db
            .lock()
            .unwrap()
            .execute("UPDATE conversations SET shared=1 WHERE id=?1", [&f.parent])
            .unwrap();
        assert_eq!(
            save(&f.parent, &id, 2).await.unwrap_err().0,
            StatusCode::FORBIDDEN
        );
        let db = f.s.db.lock().unwrap();
        assert_eq!(note(&db, &f.parent, &id).unwrap().unwrap()["revision"], 2);
    }

    #[tokio::test]
    async fn same_card_and_room_message_survive_retries_edits_reopen_and_refusals() {
        let f = fixture();
        std::fs::write(
            f.s.home.join("companion-actions.json"),
            r#"{"enabled":true}"#,
        )
        .unwrap();
        f.s.db.lock().unwrap().execute("INSERT INTO companion_tool_grants(call_id,token,port) VALUES(?1,'note-token',12345)",[&f.call]).unwrap();
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, "Bearer note-token".parse().unwrap());
        let id = Uuid::new_v4().to_string();
        let op = Uuid::new_v4().to_string();
        let create = json!({"kind":"note","title":"Shopping","text":"One apple","request_id":op,"note_id":id,"expected_revision":0});
        let show = |body: Value| {
            companion_tools::show(
                State(f.s.clone()),
                Path(f.call.clone()),
                h.clone(),
                Json(serde_json::from_value::<Show>(body).unwrap()),
            )
        };
        let first = show(create.clone()).await.unwrap().0;
        let replay = show(create.clone()).await.unwrap().0;
        assert_eq!(first["card_id"], replay["card_id"]);
        assert_eq!(replay["revision"], 1);
        assert_eq!(replay["replayed"], true);
        let mut edit = create.clone();
        edit["expected_revision"] = json!(1);
        edit["request_id"] = json!(Uuid::new_v4().to_string());
        edit["text"] = json!("One apple\nTwo oranges");
        let second = show(edit.clone()).await.unwrap().0;
        assert_eq!(second["revision"], 2);
        assert_eq!(second["card_id"], first["card_id"]);
        assert_eq!(show(create.clone()).await.unwrap().0["revision"], 1);
        assert!(show(edit.clone()).await.unwrap().0["replayed"] == true);
        let mut stale = edit.clone();
        stale["request_id"] = json!(Uuid::new_v4().to_string());
        assert_eq!(show(stale).await.unwrap_err().0, StatusCode::CONFLICT);
        let mut invalid = edit;
        invalid["text"] = json!("Different payload, reused operation");
        assert_eq!(show(invalid).await.unwrap_err().0, StatusCode::CONFLICT);
        let messages = f.s.message_page(&f.parent, None, 100, "").unwrap();
        let source = f.s.source_message_page(&f.parent, 100, None, "").unwrap();
        let item = source.iter().find(|m| m["id"] == id).unwrap();
        assert_eq!(item["room_note"], json!({"note_id":id,"revision":2}));
        let notes: Vec<_> = messages.iter().filter(|m| m["id"] == id).collect();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0]["room_note"]["revision"], 2);
        assert_eq!(notes[0]["room_note"]["text"], "One apple\nTwo oranges");
        let reopened = Connection::open(f.s.home.join("state.sqlite3")).unwrap();
        assert_eq!(
            note(&reopened, &f.parent, &id).unwrap().unwrap()["revision"],
            2
        );
        let mut db = f.s.db.lock().unwrap();
        assert!(note(&db, &f.room, &id).unwrap().is_none());
        let tx = db.transaction().unwrap();
        let cross = Edit {
            title: "x".into(),
            text: "x".into(),
            request_id: Uuid::new_v4().to_string(),
            expected_revision: 2,
        };
        assert_eq!(
            put(&tx, &f.room, &id, "default", &cross).unwrap_err().0,
            StatusCode::NOT_FOUND
        );
        let counts:(i64,i64)=tx.query_row("SELECT (SELECT count(*) FROM companion_events WHERE call_id=?1 AND kind='card'),(SELECT count(*) FROM companion_events WHERE call_id=?1 AND kind='room_note_edit')",[&f.call],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(counts, (1, 1));
    }
}
