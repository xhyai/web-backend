use axum::{
    extract::{Path, State},
    http::header,
    response::{IntoResponse, Response},
};
use std::io::{Cursor, Write};

use crate::{auth::AuthUser, error::{ApiError, ApiResult}, SharedState};

/// `GET /admin/subm/task/{task_id}` — download all submissions for a task
/// as a ZIP archive.
///
/// Implements the #191 fix: returns 404 instead of a silent empty ZIP when
/// there is nothing to download, logs warnings for files missing on disk,
/// and includes `SubmittedCode` rows (programming module answers stored in
/// the DB) as `code_<evaluation_id>.txt`.
pub async fn subm_files_task(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(task_id): Path<i64>,
) -> ApiResult<Response> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }

    let modules: Vec<(i64,)> =
        sqlx::query_as("SELECT id FROM modules WHERE task = ?")
            .bind(task_id)
            .fetch_all(&state.db)
            .await?;

    let mut buf = Cursor::new(Vec::new());
    let files_added: usize = {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default();
        let mut count = 0usize;

        for (module_id,) in &modules {
            // users with evaluations for this module
            let users: Vec<(i64, String, String)> = sqlx::query_as(
                "SELECT DISTINCT u.id, u.first_name, u.last_name
                 FROM users u JOIN evaluations e ON e.user = u.id
                 WHERE e.module = ?",
            )
            .bind(module_id)
            .fetch_all(&state.db)
            .await?;

            for (user_id, first_name, last_name) in &users {
                let userdir = format!(
                    "module_{}/{}_{}",
                    module_id,
                    strip_accents(first_name),
                    strip_accents(last_name)
                )
                .replace(' ', "_");

                // submitted files
                let files: Vec<(String, i64)> = sqlx::query_as(
                    "SELECT DISTINCT sf.path, sf.evaluation
                     FROM submitted_files sf
                     JOIN evaluations e ON e.id = sf.evaluation
                     WHERE e.user = ? AND e.module = ?",
                )
                .bind(user_id)
                .bind(module_id)
                .fetch_all(&state.db)
                .await?;

                for (path, _eval_id) in &files {
                    match std::fs::read(path) {
                        Ok(data) => {
                            let name = format!(
                                "{}/{}",
                                userdir,
                                std::path::Path::new(path)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "file".into())
                            );
                            zip.start_file(name, options).map_err(|e| {
                                ApiError::Internal(format!("ZIP error: {e}"))
                            })?;
                            zip.write_all(&data).map_err(|e| {
                                ApiError::Internal(format!("ZIP error: {e}"))
                            })?;
                            count += 1;
                        }
                        Err(_) => {
                            tracing::warn!(
                                "submitted file missing on disk, skipping: {path}"
                            );
                        }
                    }
                }

                // submitted code (programming modules) from the DB
                let codes: Vec<(i64, String)> = sqlx::query_as(
                    "SELECT sc.evaluation, sc.code
                     FROM submitted_codes sc
                     JOIN evaluations e ON e.id = sc.evaluation
                     WHERE e.user = ? AND e.module = ?",
                )
                .bind(user_id)
                .bind(module_id)
                .fetch_all(&state.db)
                .await?;

                for (eval_id, code) in &codes {
                    let name = format!("{}/code_{eval_id}.txt", userdir);
                    zip.start_file(name, options).map_err(|e| {
                        ApiError::Internal(format!("ZIP error: {e}"))
                    })?;
                    zip.write_all(code.as_bytes()).map_err(|e| {
                        ApiError::Internal(format!("ZIP error: {e}"))
                    })?;
                    count += 1;
                }
            }
        }

        if count == 0 {
            return Err(ApiError::NotFound(
                "Pro tuto úlohu nebyla nalezena žádná odevzdání ke stažení.".into(),
            ));
        }
        zip.finish().map_err(|e| ApiError::Internal(format!("ZIP error: {e}")))?;
        count
    };

    let _ = files_added;
    let bytes = buf.into_inner();
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip"),
            (
                header::CONTENT_DISPOSITION,
                &format!("inline; filename=\"task_{task_id}.zip\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Strip Czech/Slovak diacritics for ZIP entry names (mirrors
/// `util.submissions.strip_accents`).
fn strip_accents(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' => 'a',
            'č' => 'c',
            'ď' => 'd',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'ě' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ľ' => 'l',
            'ň' => 'n',
            'ó' | 'ò' | 'ô' | 'ö' => 'o',
            'ř' => 'r',
            'š' => 's',
            'ť' => 't',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ů' => 'u',
            'ý' => 'y',
            'ž' => 'z',
            'Á' | 'À' | 'Â' | 'Ä' => 'A',
            'Č' => 'C',
            'Ď' => 'D',
            'É' | 'È' | 'Ê' | 'Ë' => 'E',
            'Ě' => 'E',
            'Í' | 'Ì' | 'Î' | 'Ï' => 'I',
            'Ľ' => 'L',
            'Ň' => 'N',
            'Ó' | 'Ò' | 'Ô' | 'Ö' => 'O',
            'Ř' => 'R',
            'Š' => 'S',
            'Ť' => 'T',
            'Ú' | 'Ù' | 'Û' | 'Ü' => 'U',
            'Ů' => 'U',
            'Ý' => 'Y',
            'Ž' => 'Z',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents_stripped() {
        assert_eq!(strip_accents("Příliš žluťoučký kůň"), "Prilis zlutoucky kun");
    }
}
