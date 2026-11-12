use crate::{
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use rand::RngCore;
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(all(unix, not(target_os = "macos")))]
use std::ffi::{CStr, CString};
use std::sync::Arc;
#[cfg(unix)]
use zeroize::Zeroizing;

#[cfg(all(unix, not(target_os = "macos")))]
struct PasswordConversation {
    password: Zeroizing<String>,
    answered: bool,
}
#[cfg(all(unix, not(target_os = "macos")))]
impl pam_client::ConversationHandler for PasswordConversation {
    fn prompt_echo_on(&mut self, _: &CStr) -> Result<CString, pam_client::ErrorCode> {
        Err(pam_client::ErrorCode::CONV_ERR)
    }
    fn prompt_echo_off(&mut self, prompt: &CStr) -> Result<CString, pam_client::ErrorCode> {
        // This first UI supports a single ordinary password, not MFA or password changes.
        if self.answered
            || !prompt
                .to_string_lossy()
                .trim()
                .eq_ignore_ascii_case("password:")
        {
            return Err(pam_client::ErrorCode::CONV_ERR);
        }
        self.answered = true;
        CString::new(self.password.as_bytes()).map_err(|_| pam_client::ErrorCode::CONV_ERR)
    }
    fn text_info(&mut self, _: &CStr) {}
    fn error_msg(&mut self, _: &CStr) {}
}

/// Authenticate first, then return the configured actor used in rooms/audiences.
/// Windows accepts OS account aliases only when their authenticated SID matches
/// an explicitly configured account. Additional users are not restricted to the
/// service process owner, and are not elevated to administrator by an alias.
pub fn authenticate_allowed(
    service: &str,
    username: &str,
    password: String,
    allowed: &[String],
) -> Option<String> {
    #[cfg(windows)]
    {
        let _ = service;
        crate::platform::windows::authenticate_allowed(username, password, allowed)
    }
    #[cfg(unix)]
    {
        let password = Zeroizing::new(password);
        let actor = allowed.iter().find(|actor| actor.as_str() == username)?;
        #[cfg(target_os = "macos")]
        let success = crate::platform::macos::authenticate(service, username, password.to_string());
        #[cfg(not(target_os = "macos"))]
        let success = {
            let conversation = PasswordConversation {
                password,
                answered: false,
            };
            let Ok(mut context) = pam_client::Context::new(service, Some(username), conversation)
            else {
                return None;
            };
            context.authenticate(pam_client::Flag::NONE).is_ok()
                && context.acct_mgmt(pam_client::Flag::NONE).is_ok()
        };
        success.then(|| actor.clone())
    }
}

pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub fn cookie_name(state: &AppState) -> String {
    format!("acv_{}", state.config.id.replace('-', ""))
}
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        // HTTP/2 may send each cookie in its own field. Other apps on the
        // same hostname (including different ports) can precede our cookie.
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|v| {
            let (k, v) = v.trim().split_once('=')?;
            (k == name).then(|| v.to_owned())
        })
}
fn constant_equal(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |sum, (a, b)| sum | (a ^ b))
            == 0
}
#[derive(Clone)]
pub struct Principal {
    pub actor: String,
    pub admin: bool,
    pub csrf: Option<String>,
    pub session_hash: Option<String>,
}

pub fn check(
    state: &AppState,
    headers: &HeaderMap,
    scope: &str,
    mutation: bool,
) -> ApiResult<Principal> {
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        let token = value
            .to_str()
            .ok()
            .and_then(|s| s.strip_prefix("Bearer "))
            .ok_or_else(ApiError::unauthorized)?;
        let row: Option<(String, String)> = state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT id,scopes FROM tokens WHERE hash=?1 AND expires_at>?2",
                params![digest(token), now_ms()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (id, scopes) = row.ok_or_else(ApiError::unauthorized)?;
        let scopes: Vec<String> = serde_json::from_str(&scopes).unwrap_or_default();
        if !scopes.iter().any(|s| s == "admin" || s == scope) {
            return Err(ApiError::forbidden());
        }
        return Ok(Principal {
            actor: if scopes.iter().any(|s| s == "admin") {
                state.config.owner.clone()
            } else {
                format!("token:{id}")
            },
            admin: scopes.iter().any(|s| s == "admin"),
            csrf: None,
            session_hash: None,
        });
    }
    let token = cookie_value(headers, &cookie_name(state)).ok_or_else(ApiError::unauthorized)?;
    let hash = digest(&token);
    let row: Option<(String, String)> = state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT owner,csrf FROM browser_sessions WHERE hash=?1 AND expires_at>?2",
            params![hash, now_ms()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (owner, csrf) = row.ok_or_else(ApiError::unauthorized)?;
    if !state.config.allows_user(&owner) {
        return Err(ApiError::unauthorized());
    }
    let admin = owner == state.config.owner;
    if scope == "admin" && !admin {
        return Err(ApiError::forbidden());
    }
    // Browser sessions do not inherit machine-only provider/import scopes.
    if !["admin", "observe", "chat"].contains(&scope) {
        return Err(ApiError::forbidden());
    }
    if mutation {
        same_origin(headers)?;
        if !headers
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| constant_equal(v, &csrf))
        {
            return Err(ApiError::forbidden());
        }
    }
    Ok(Principal {
        actor: owner,
        admin,
        csrf: Some(csrf),
        session_hash: Some(hash),
    })
}
pub fn same_origin(headers: &HeaderMap) -> ApiResult<()> {
    if let Some(origin) = headers.get(header::ORIGIN) {
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(ApiError::forbidden)?;
        if origin.to_str().ok() != Some(format!("https://{host}").as_str()) {
            return Err(ApiError::forbidden());
        }
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Login {
    username: String,
    password: String,
}
pub async fn login(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(login): Json<Login>,
) -> ApiResult<Response> {
    same_origin(&headers)?;
    if login.username.len() > 100 || login.password.len() > 1024 || login.password.is_empty() {
        return Err(ApiError::unauthorized());
    }
    let permit = state.login_gate.clone().try_acquire_owned().map_err(|_| {
        ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Please wait before trying to sign in again".into(),
        )
    })?;
    let config = state.config.clone();
    {
        let db = state.db.lock().unwrap();
        let locked: Option<i64> = db
            .query_row(
                "SELECT locked_until FROM login_attempts WHERE key='owner'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if locked.is_some_and(|until| until > now_ms()) {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Please wait before trying to sign in again".into(),
            ));
        }
        db.execute(
            "DELETE FROM browser_sessions WHERE expires_at<=?1",
            [now_ms()],
        )?;
    }
    let mut allowed = vec![config.owner.clone()];
    allowed.extend(config.allowed_users.iter().cloned());
    let service = config.pam_service;
    let worker = tokio::task::spawn_blocking(move || {
        // The permit remains held even if an OS login outlives the HTTP timeout.
        let _permit = permit;
        authenticate_allowed(&service, &login.username, login.password, &allowed)
    });
    let actor = tokio::time::timeout(std::time::Duration::from_secs(20), worker)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    let Some(owner) = actor else {
        state.db.lock().unwrap().execute("INSERT INTO login_attempts(key,failures,locked_until) VALUES('owner',1,0) ON CONFLICT(key) DO UPDATE SET failures=CASE WHEN locked_until>0 AND locked_until<?1 THEN 1 ELSE failures+1 END, locked_until=CASE WHEN failures>=4 THEN ?1+60000 ELSE 0 END",[now_ms()])?;
        state.audit(
            "auth.failed",
            None,
            json!({"method":crate::platform::auth_method()}),
        )?;
        return Err(ApiError::unauthorized());
    };
    let token = random_token();
    let csrf = random_token();
    {
        let db = state.db.lock().unwrap();
        db.execute("DELETE FROM login_attempts WHERE key='owner'", [])?;
        db.execute(
            "INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,?2,?3,?4)",
            params![digest(&token), owner, csrf, now_ms() + 8 * 3600 * 1000],
        )?;
    }
    state.audit(
        "auth.signed_in",
        None,
        json!({"actor":owner,"method":crate::platform::auth_method()}),
    )?;
    let cookie = format!(
        "{}={}; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=28800",
        cookie_name(&state),
        token
    );
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({"authenticated":true,"owner":owner,"admin":owner == state.config.owner,"csrf":csrf})),
    )
        .into_response())
}
pub async fn session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    match check(&state, &headers, "observe", false) {
        Ok(p) => Ok(Json(
            json!({"authenticated":true,"owner":p.actor,"admin":p.admin,"csrf":p.csrf}),
        )),
        Err(e) if e.0 == StatusCode::UNAUTHORIZED => Ok(Json(
            json!({"authenticated":false,"owner":state.config.owner,"csrf":null}),
        )),
        Err(e) => Err(e),
    }
}
pub async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Response> {
    let p = check(&state, &headers, "observe", true)?;
    if let Some(hash) = p.session_hash {
        state
            .db
            .lock()
            .unwrap()
            .execute("DELETE FROM browser_sessions WHERE hash=?1", [hash])?;
    }
    state.audit("auth.signed_out", None, json!({"actor":p.actor}))?;
    Ok((
        [(
            header::SET_COOKIE,
            format!(
                "{}=; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=0",
                cookie_name(&state)
            ),
        )],
        Json(json!({"ok":true})),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(unix, not(target_os = "macos")))]
    use pam_client::ConversationHandler;
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn password_is_not_reused_for_another_authentication_challenge() {
        let mut c = PasswordConversation {
            password: Zeroizing::new("test".into()),
            answered: false,
        };
        assert!(c.prompt_echo_off(c"Verification code:").is_err());
        assert!(c.prompt_echo_off(c"Password:").is_ok());
        assert!(c.prompt_echo_off(c"Password:").is_err());
        assert!(c.prompt_echo_on(c"Username:").is_err());
    }
    #[test]
    fn unlisted_accounts_fail_without_creating_a_session() {
        assert_eq!(
            authenticate_allowed(
                "agent-conversations-no-such-policy",
                "unlisted",
                "fixture".into(),
                &[]
            ),
            None
        );
    }
}
