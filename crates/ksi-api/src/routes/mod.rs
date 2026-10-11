pub mod admin;
pub mod articles;
pub mod auth;
pub mod posts;
pub mod registration;
pub mod tasks;
pub mod threads;
pub mod waves;
pub mod years;

use axum::{routing::get, Router};

use crate::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new()
        // years
        .route("/years", get(years::list_years))
        .route("/years/:id", get(years::get_year))
        // waves
        .route("/waves", get(waves::list_waves))
        .route("/waves/:id", get(waves::get_wave))
        // articles
        .route("/articles", get(articles::list_articles))
        .route("/articles/:id", get(articles::get_article))
        // auth
        .route("/auth", axum::routing::post(auth::authorize))
        .route("/logout", get(auth::logout))
        // posts & threads
        .route("/posts", axum::routing::post(posts::create_post))
        .route("/posts/:id", get(posts::get_post))
        .route("/posts/:id/resolve", axum::routing::post(posts::resolve_post).delete(posts::unresolve_post))
        .route("/threads", get(threads::list_threads))
        .route("/threads/:id", get(threads::get_thread))
        .route("/threadDetails/:id", get(threads::get_thread_details))
        // tasks
        .route("/tasks", get(tasks::list_tasks))
        .route("/tasks/:id", get(tasks::get_task))
        // registration
        .route("/registration", axum::routing::post(registration::register))
        .route(
            "/forgottenPassword",
            axum::routing::post(registration::forgotten_password),
        )
        // admin
        .route("/admin/subm/task/:id", get(admin::subm_files_task))
        // misc
        .route("/robots.txt", get(|| async { "User-agent: *\nDisallow:\n" }))
        .route("/csp", get(|| async { axum::Json(serde_json::json!({})) }))
}
