use std::collections::HashMap;

use axum::{
    extract::{Multipart, Path, State},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use ksi_auth::AuthUser;
use ksi_models::{
    round1, BasicProfileJson, NotifyRow, ProfileJson, ProfileRow, TaskScoreJson, UserRow,
};
use crate::{
    error::{ApiError, ApiResult},
    routes::tasks,
    SharedState,
};

/// Mirror of `util.user.get_profile_picture`: the `/images/profile/<id>`
/// URL is only advertised when the file actually exists on disk.
pub(crate) fn profile_picture_url(user: &UserRow) -> Option<String> {
    match &user.profile_picture {
        Some(p) if std::path::Path::new(p).exists() => {
            Some(format!("/images/profile/{}", user.id))
        }
        _ => None,
    }
}

/// Current year: the `YEAR` request header wins (mirrors the Python
/// middleware; also documented in the swagger spec for `GET /profile`),
/// otherwise the newest year.
pub(crate) async fn current_year(db: &sqlx::SqlitePool, headers: &HeaderMap) -> ApiResult<i64> {
    if let Some(raw) = headers.get("year").and_then(|v| v.to_str().ok()) {
        if let Ok(want) = raw.trim().parse::<i64>() {
            let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM years WHERE id = ?")
                .bind(want)
                .fetch_optional(db)
                .await?;
            if let Some(id) = exists {
                return Ok(id);
            }
        }
    }
    let id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM years ORDER BY id DESC LIMIT 1")
            .fetch_optional(db)
            .await?;
    id.ok_or_else(|| ApiError::BadRequest("Není dostupný žádný ročník.".into()))
}

struct ProfileData {
    user: UserRow,
    profile: Option<ProfileRow>,
    notify: Option<NotifyRow>,
}

pub(crate) const USER_COLS: &str = "id, email, github, discord, first_name, nick_name, last_name, sex, short_info, profile_picture, role, enabled";
pub(crate) const PROFILE_COLS: &str = "user_id, addr_street, addr_city, addr_zip, addr_country, school_name, school_street, school_city, school_zip, school_country, school_finish, tshirt_size";
const NOTIFY_COLS: &str = "user, auth_token, notify_eval, notify_response, notify_ksi, notify_events";

async fn load_profile_data(
    db: &sqlx::SqlitePool,
    user_id: i64,
) -> ApiResult<Option<ProfileData>> {
    let user: Option<UserRow> =
        sqlx::query_as(&format!("SELECT {USER_COLS} FROM users WHERE id = ?"))
            .bind(user_id)
            .fetch_optional(db)
            .await?;
    let user = match user {
        Some(u) => u,
        None => return Ok(None),
    };
    let profile: Option<ProfileRow> =
        sqlx::query_as(&format!("SELECT {PROFILE_COLS} FROM profiles WHERE user_id = ?"))
            .bind(user_id)
            .fetch_optional(db)
            .await?;
    let notify: Option<NotifyRow> =
        sqlx::query_as(&format!("SELECT {NOTIFY_COLS} FROM users_notify WHERE user = ?"))
            .bind(user_id)
            .fetch_optional(db)
            .await?;
    Ok(Some(ProfileData { user, profile, notify }))
}

/// Mirror of `util/profile.py::to_json`.
///
/// `sensitive` adds the street address fields (own profile, or an admin
/// looking at someone else's — mirrors the Python `sensitive` flag).
/// The object shape follows the swagger `Profile` schema exactly.
fn profile_to_json(data: &ProfileData, sensitive: bool) -> ProfileJson {
    let u = &data.user;
    let p = data.profile.as_ref();
    let s = |f: fn(&ProfileRow) -> &String| {
        p.map(f).cloned().unwrap_or_default()
    };
    let (addr_street, addr_city, addr_zip) = if sensitive {
        match p {
            Some(pr) => (
                Some(pr.addr_street.clone()),
                Some(pr.addr_city.clone()),
                Some(pr.addr_zip.clone()),
            ),
            None => (None, None, None),
        }
    } else {
        (None, None, None)
    };
    let n = data.notify.as_ref();
    ProfileJson {
        id: u.id,
        author: u.id,
        resource: format!("/profile/{}", u.id),
        addr_street,
        addr_city,
        addr_zip,
        email: u.email.clone(),
        nick_name: u.nick_name.clone(),
        first_name: u.first_name.clone(),
        last_name: u.last_name.clone(),
        gender: u.sex.clone(),
        short_info: u.short_info.clone(),
        github: u.github.clone(),
        discord: u.discord.clone(),
        addr_country: s(|x| &x.addr_country),
        school_name: s(|x| &x.school_name),
        school_street: s(|x| &x.school_street),
        school_city: s(|x| &x.school_city),
        school_zip: s(|x| &x.school_zip),
        school_country: s(|x| &x.school_country),
        school_finish: p.map(|x| x.school_finish).unwrap_or(0),
        tshirt_size: s(|x| &x.tshirt_size),
        notify_eval: n.map(|x| x.notify_eval).unwrap_or(true),
        notify_response: n.map(|x| x.notify_response).unwrap_or(true),
        notify_ksi: n.map(|x| x.notify_ksi).unwrap_or(true),
        notify_events: n.map(|x| x.notify_events).unwrap_or(true),
    }
}

/// Submitted tasks + per-task scores for the profile page
/// (mirrors `util/task.py::any_submitted` + `util/profile.py::task_score_to_json`).
async fn profile_tasks(
    db: &sqlx::SqlitePool,
    user_id: i64,
    year_id: i64,
) -> ApiResult<(Vec<tasks::TaskJson>, Vec<TaskScoreJson>)> {
    // Tasks the user submitted anything into this year.
    let submitted: Vec<(i64, bool)> = sqlx::query_as(
        "SELECT DISTINCT t.id, t.evaluation_public
         FROM tasks t
         JOIN modules m ON m.task = t.id
         JOIN evaluations e ON e.module = m.id
         JOIN waves w ON w.id = t.wave
         WHERE e.user = ? AND w.year = ?
         ORDER BY t.id",
    )
    .bind(user_id)
    .bind(year_id)
    .fetch_all(db)
    .await?;

    // Per-task sum of best-per-module points (any modules, like Python).
    let points: Vec<(i64, Option<f64>)> = sqlx::query_as(
        "SELECT m.task, SUM(pm.pts)
         FROM (SELECT e.module AS module, MAX(e.points) AS pts
               FROM evaluations e WHERE e.user = ? GROUP BY e.module) pm
         JOIN modules m ON m.id = pm.module
         GROUP BY m.task",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    let points: HashMap<i64, f64> = points
        .into_iter()
        .map(|(t, s)| (t, s.unwrap_or(0.0)))
        .collect();

    // Achievements granted for each task.
    let achs: Vec<(Option<i64>, i64)> = sqlx::query_as(
        "SELECT task_id, achievement_id FROM user_achievement
         WHERE user_id = ? AND task_id IS NOT NULL",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    let mut ach_map: HashMap<i64, Vec<i64>> = HashMap::new();
    for (task_id, ach_id) in achs {
        if let Some(t) = task_id {
            ach_map.entry(t).or_default().push(ach_id);
        }
    }

    let mut task_jsons = Vec::with_capacity(submitted.len());
    let mut scores = Vec::with_capacity(submitted.len());
    for (task_id, evaluation_public) in &submitted {
        if let Some(row) = sqlx::query_as::<_, tasks::TaskRow>(
            "SELECT t.id, t.title, t.author, t.co_author, t.wave, t.intro,
                    t.time_deadline, t.picture_base
             FROM tasks t WHERE t.id = ?",
        )
        .bind(task_id)
        .fetch_optional(db)
        .await?
        {
            let wp = tasks::wave_published(db, row.wave).await;
            task_jsons.push(tasks::task_to_json(db, &row, wp).await);
        }
        scores.push(TaskScoreJson {
            id: *task_id,
            task: *task_id,
            achievements: ach_map.remove(task_id).unwrap_or_default(),
            score: evaluation_public.then(|| round1(points.get(task_id).copied().unwrap_or(0.0))),
        });
    }
    Ok((task_jsons, scores))
}

#[derive(Serialize)]
pub struct ProfileResponse {
    pub profile: ProfileJson,
    pub tasks: Vec<tasks::TaskJson>,
    #[serde(rename = "taskScores")]
    pub task_scores: Vec<TaskScoreJson>,
}

async fn full_profile_response(
    db: &sqlx::SqlitePool,
    headers: &HeaderMap,
    data: &ProfileData,
    sensitive: bool,
) -> ApiResult<ProfileResponse> {
    let year_id = current_year(db, headers).await?;
    let (tasks, task_scores) = profile_tasks(db, data.user.id, year_id).await?;
    Ok(ProfileResponse {
        profile: profile_to_json(data, sensitive),
        tasks,
        task_scores,
    })
}

/// `GET /profile` — current user's full profile.
/// Not logged in → the fake profile (mirrors `util/profile.py::fake_profile`).
pub async fn get_profile(
    State(state): State<SharedState>,
    headers: HeaderMap,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() {
        return Ok(Json(json!({"profile": {"id": 0, "signed_in": false}})));
    }
    let data = load_profile_data(&state.db, user.id.unwrap())
        .await?
        .ok_or_else(|| ApiError::NotFound("Uživatel neexistuje.".into()))?;
    let resp = full_profile_response(&state.db, &headers, &data, true).await?;
    Ok(Json(json!(resp)))
}

/// `GET /basicProfile` — minimal profile info for the navbar.
pub async fn get_basic_profile(
    State(state): State<SharedState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() {
        return Ok(Json(json!({"profile": {"id": 0, "signed_in": false}})));
    }
    let data = load_profile_data(&state.db, user.id.unwrap())
        .await?
        .ok_or_else(|| ApiError::NotFound("Uživatel neexistuje.".into()))?;
    let u = &data.user;
    Ok(Json(json!({
        "basicProfile": BasicProfileJson {
            id: u.id,
            first_name: u.first_name.clone(),
            last_name: u.last_name.clone(),
            nick_name: u.nick_name.clone().unwrap_or_default(),
            // swagger requires a string; the frontend falls back to a
            // default avatar for falsy values
            profile_picture: profile_picture_url(u).unwrap_or_default(),
            short_info: u.short_info.clone(),
            email: u.email.clone(),
            gender: u.sex.clone(),
            role: u.role.clone(),
            signed_in: true,
        }
    })))
}

/// `GET /profile/{id}` — someone else's profile (orgs only; street
/// address visible to admins only). Mirrors `endpoint/profile.py::OrgProfile`.
pub async fn get_org_profile(
    State(state): State<SharedState>,
    headers: HeaderMap,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() || !user.is_org() {
        // Python answered HTTP 400 here with a '401' error object; use a
        // proper 401 with the same envelope and message.
        return Err(ApiError::Unauthorized(
            "Prohlížet cizí profily může pouze administrátor.".into(),
        ));
    }
    let mut data = load_profile_data(&state.db, id).await?.ok_or_else(|| {
        ApiError::NotFound("Uživatel nebo profil s tímto ID neexistuje.".into())
    })?;
    if data.profile.is_none() {
        return Err(ApiError::NotFound(
            "Uživatel nebo profil s tímto ID neexistuje.".into(),
        ));
    }
    // NOTE: mirrors Python, which serializes the *viewer's* notify settings
    // into the viewed profile response.
    let viewer_notify: Option<NotifyRow> =
        sqlx::query_as(&format!("SELECT {NOTIFY_COLS} FROM users_notify WHERE user = ?"))
            .bind(user.id.unwrap())
            .fetch_optional(&state.db)
            .await?;
    data.notify = viewer_notify;
    let resp = full_profile_response(&state.db, &headers, &data, user.is_admin()).await?;
    Ok(Json(json!(resp)))
}

/// Request body for `PUT /profile` (mirrors the swagger `ProfileEdit` schema).
#[derive(Debug, Deserialize)]
pub struct ProfileEdit {
    pub email: String,
    pub nick_name: Option<String>,
    pub first_name: String,
    pub last_name: String,
    pub gender: String,
    pub short_info: String,
    pub github: Option<String>,
    pub discord: Option<String>,
    pub addr_street: String,
    pub addr_city: String,
    pub addr_zip: String,
    pub addr_country: String,
    pub school_name: String,
    pub school_street: String,
    pub school_city: String,
    pub school_zip: String,
    pub school_country: String,
    pub school_finish: i64,
    pub tshirt_size: String,
    pub notify_eval: Option<bool>,
    pub notify_response: Option<bool>,
    pub notify_ksi: Option<bool>,
    pub notify_events: Option<bool>,
}

fn new_notify_token() -> String {
    // Mirrors util/user_notify.py::new_token (40 hex chars).
    let a = uuid::Uuid::new_v4().simple().to_string();
    let b = uuid::Uuid::new_v4().simple().to_string();
    format!("{a}{b}")[..40].to_string()
}

/// `PUT /profile` — update the current user's profile.
///
/// Deviations from Python: the `audit_log` entries (name/school/discord
/// changes) and the Discord username-change webhook POST are not ported —
/// the Rust workspace has no audit/webhook infrastructure yet.
pub async fn update_profile(
    State(state): State<SharedState>,
    headers: HeaderMap,
    user: AuthUser,
    Json(body): Json<ProfileEdit>,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }
    let user_id = user.id.unwrap();

    sqlx::query(
        "UPDATE users SET first_name = ?, nick_name = ?, last_name = ?, email = ?,
                          sex = ?, short_info = ?, github = ?, discord = ?
         WHERE id = ?",
    )
    .bind(&body.first_name)
    .bind(body.nick_name.clone())
    .bind(&body.last_name)
    .bind(&body.email)
    .bind(&body.gender)
    .bind(&body.short_info)
    .bind(body.github.clone())
    .bind(body.discord.clone())
    .bind(user_id)
    .execute(&state.db)
    .await?;

    let profile_exists: Option<i64> =
        sqlx::query_scalar("SELECT user_id FROM profiles WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await?;
    if profile_exists.is_some() {
        sqlx::query(
            "UPDATE profiles SET addr_street = ?, addr_city = ?, addr_zip = ?, addr_country = ?,
                               school_name = ?, school_street = ?, school_city = ?, school_zip = ?,
                               school_country = ?, school_finish = ?, tshirt_size = ?
             WHERE user_id = ?",
        )
        .bind(&body.addr_street)
        .bind(&body.addr_city)
        .bind(&body.addr_zip)
        .bind(body.addr_country.to_lowercase())
        .bind(&body.school_name)
        .bind(&body.school_street)
        .bind(&body.school_city)
        .bind(&body.school_zip)
        .bind(body.school_country.to_lowercase())
        .bind(body.school_finish)
        .bind(&body.tshirt_size)
        .bind(user_id)
        .execute(&state.db)
        .await?;
    } else {
        sqlx::query(
            "INSERT INTO profiles (user_id, addr_street, addr_city, addr_zip, addr_country,
                                   school_name, school_street, school_city, school_zip,
                                   school_country, school_finish, tshirt_size)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(user_id)
        .bind(&body.addr_street)
        .bind(&body.addr_city)
        .bind(&body.addr_zip)
        .bind(body.addr_country.to_lowercase())
        .bind(&body.school_name)
        .bind(&body.school_street)
        .bind(&body.school_city)
        .bind(&body.school_zip)
        .bind(body.school_country.to_lowercase())
        .bind(body.school_finish)
        .bind(&body.tshirt_size)
        .execute(&state.db)
        .await?;
    }

    let notify_exists: Option<i64> =
        sqlx::query_scalar("SELECT user FROM users_notify WHERE user = ?")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await?;
    let (ne, nr, nk, nev) = (
        body.notify_eval.unwrap_or(true),
        body.notify_response.unwrap_or(true),
        body.notify_ksi.unwrap_or(true),
        body.notify_events.unwrap_or(true),
    );
    if notify_exists.is_some() {
        sqlx::query(
            "UPDATE users_notify SET notify_eval = ?, notify_response = ?,
                                     notify_ksi = ?, notify_events = ?
             WHERE user = ?",
        )
        .bind(ne)
        .bind(nr)
        .bind(nk)
        .bind(nev)
        .bind(user_id)
        .execute(&state.db)
        .await?;
    } else {
        sqlx::query(
            "INSERT INTO users_notify (user, auth_token, notify_eval, notify_response,
                                       notify_ksi, notify_events)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(user_id)
        .bind(new_notify_token())
        .bind(ne)
        .bind(nr)
        .bind(nk)
        .bind(nev)
        .execute(&state.db)
        .await?;
    }

    let data = load_profile_data(&state.db, user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Uživatel neexistuje.".into()))?;
    let resp = full_profile_response(&state.db, &headers, &data, true).await?;
    Ok(Json(json!(resp)))
}

// ---------------------------------------------------------------------------
// Profile picture upload
// ---------------------------------------------------------------------------

const MAX_PICTURE_BYTES: usize = 10 * 1024 * 1024;
const PICTURE_DIR: &str = "data/images/profile";

/// Magic-byte sniffing (mirrors the `magic` + `ALLOWED_MIME_TYPES` check in
/// `endpoint/profile.py`).
fn sniff_image_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        Some("jpg")
    } else if bytes.len() >= 8
        && bytes[0..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
    {
        Some("png")
    } else if bytes.len() >= 6
        && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a")
    {
        Some("gif")
    } else {
        None
    }
}

/// `POST /profile/picture` — upload a profile picture (multipart `file`
/// field, JPEG/PNG/GIF only).
///
/// Deviation from Python: the image is stored as uploaded, without the
/// PIL center-crop to a 263×263 square thumbnail — no image-processing
/// crate is available in the offline build environment.
pub async fn upload_picture(
    State(state): State<SharedState>,
    user: AuthUser,
    mut multipart: Multipart,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }
    let user_id = user.id.unwrap();

    let mut file_bytes: Option<Vec<u8>> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Neplatný multipart: {e}")))?
    {
        if field.name() == Some("file") {
            let bytes = field
                .bytes()
                .await
                .map_err(|e| ApiError::BadRequest(format!("Nelze přečíst soubor: {e}")))?;
            if bytes.len() > MAX_PICTURE_BYTES {
                return Err(ApiError::PayloadTooLarge(
                    "Obrázek je příliš velký (max 10 MB).".into(),
                ));
            }
            file_bytes = Some(bytes.to_vec());
        }
    }
    let bytes =
        file_bytes.ok_or_else(|| ApiError::BadRequest("Chybí soubor 'file'.".into()))?;
    let ext =
        sniff_image_ext(&bytes).ok_or_else(|| ApiError::BadRequest("Nepodporovaný formát obrázku.".into()))?;

    std::fs::create_dir_all(PICTURE_DIR)
        .map_err(|_| ApiError::Internal("Nelze vytvořit adresář pro profilové obrázky.".into()))?;
    let dest = format!("{PICTURE_DIR}/user_{user_id}.{ext}");
    std::fs::write(&dest, &bytes)
        .map_err(|_| ApiError::Internal("Nelze uložit profilový obrázek.".into()))?;

    sqlx::query("UPDATE users SET profile_picture = ? WHERE id = ?")
        .bind(&dest)
        .bind(user_id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({})))
}
