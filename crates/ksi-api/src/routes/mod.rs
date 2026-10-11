pub mod achievements;
pub mod admin;
pub mod articles;
pub mod atask_deploy;
pub mod atasks;
pub mod auth;
pub mod content;
pub mod corrections;
pub mod diplomas;
pub mod email;
pub mod feedback;
pub mod modules;
pub mod posts;
pub mod profile;
pub mod registration;
pub mod tasks;
pub mod threads;
pub mod users;
pub mod waves;
pub mod years;

use axum::{
    routing::{get, post},
    Router,
};

use crate::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new()
        // years
        .route("/years", get(years::list_years).post(years::create_year))
        .route(
            "/years/:id",
            get(years::get_year)
                .put(years::update_year)
                .delete(years::delete_year),
        )
        // waves
        .route("/waves", get(waves::list_waves).post(waves::create_wave))
        .route(
            "/waves/:id",
            get(waves::get_wave)
                .put(waves::update_wave)
                .delete(waves::delete_wave),
        )
        // articles
        .route(
            "/articles",
            get(articles::list_articles).post(articles::create_article),
        )
        .route(
            "/articles/:id",
            get(articles::get_article)
                .put(articles::update_article)
                .delete(articles::delete_article),
        )
        // achievements
        .route("/achievements", get(achievements::list_achievements).post(achievements::create_achievement))
        .route("/achievements/special/successful", get(achievements::successful_achievement))
        .route("/achievements/:id", get(achievements::get_achievement).put(achievements::update_achievement).delete(achievements::delete_achievement))
        .route("/admin/achievements/grant", axum::routing::post(achievements::grant_achievements))
        // feedback
        .route("/feedback", axum::routing::post(feedback::send_feedback_email))
        .route("/feedbacks", axum::routing::post(feedback::create_task_feedback))
        .route("/feedbacks/:id", get(feedback::get_task_feedback).put(feedback::update_task_feedback).delete(feedback::delete_task_feedback))
        // auth
        .route("/auth", axum::routing::post(auth::authorize))
        .route("/logout", get(auth::logout))
        // posts & threads
        .route("/posts", post(posts::create_post))
        .route(
            "/posts/:id",
            get(posts::get_post)
                .put(posts::update_post)
                .delete(posts::delete_post),
        )
        .route(
            "/posts/:id/resolve",
            post(posts::resolve_post).delete(posts::unresolve_post),
        )
        .route(
            "/threads",
            get(threads::list_threads).post(threads::create_thread),
        )
        .route(
            "/threads/:id",
            get(threads::get_thread)
                .put(threads::mark_thread_visited)
                .delete(threads::delete_thread),
        )
        .route("/threadDetails/:id", get(threads::get_thread_details))
        // tasks
        .route("/tasks", get(tasks::list_tasks))
        .route("/tasks/:id", get(tasks::get_task))
        .route("/taskDetails/:id", get(content::get_task_details))
        // content & images
        .route(
            "/content",
            get(content::get_content)
                .post(content::create_content)
                .delete(content::delete_content),
        )
        .route("/images/:context/:id", get(content::get_image))
        .route("/task-content/:id/:view", get(content::get_task_content))
        .route("/taskContent/:id", get(content::get_task_content_legacy))
        .route("/taskContent/:id/:view", get(content::get_task_content))
        // submission files
        .route(
            "/submFiles/:id",
            get(content::get_subm_file).delete(content::delete_subm_file),
        )
        // profiles & users
        .route("/profile", get(profile::get_profile).put(profile::update_profile))
        .route("/profile/picture", post(profile::upload_picture))
        .route("/profile/:id", get(profile::get_org_profile))
        .route("/basicProfile", get(profile::get_basic_profile))
        .route("/users", get(users::list_users))
        .route("/users/:id", get(users::get_user).delete(users::delete_user))
        .route("/users/:id/discord", get(users::discord_invite))
        .route(
            "/settings/changePassword",
            post(users::change_password),
        )
        // registration
        .route("/registration", axum::routing::post(registration::register))
        .route(
            "/forgottenPassword",
            axum::routing::post(registration::forgotten_password),
        )
        // admin
        .route("/admin/subm/task/:id", get(admin::subm_files_task))
        .route("/admin/subm/eval/:eval_id", get(admin::subm_files_eval))
        .route("/admin/evalCodes/:id", get(admin::eval_code))
        .route("/admin/evaluations/:id", get(admin::get_evaluation))
        .route("/admin/execs", get(admin::list_execs))
        .route("/admin/execs/:id", get(admin::get_exec))
        // admin tasks (task deploy group)
        .route(
            "/admin/atasks",
            get(atasks::list_atasks).post(atasks::create_atask),
        )
        .route(
            "/admin/atasks/:id",
            get(atasks::get_atask)
                .put(atasks::update_atask)
                .delete(atasks::delete_atask),
        )
        .route(
            "/admin/atasks/:id/deploy",
            get(atasks::deploy_status).post(atasks::deploy_atask),
        )
        .route(
            "/admin/atasks/:id/merge",
            axum::routing::post(atasks::merge_atask),
        )
        .route(
            "/admin/waves/:id/diff",
            get(atasks::wave_diff).post(atasks::wave_diff),
        )
        .route(
            "/admin/instanceConfig",
            get(admin::get_instance_config).post(admin::set_instance_config),
        )
        .route("/admin/monitoring-dashboard", get(admin::monitoring_dashboard))
        .route("/admin/user-export", get(admin::user_export))
        .route("/admin/e-mail", axum::routing::post(email::send_email))
        .route("/admin/diploma/:user_id/grant", axum::routing::post(diplomas::grant_diploma))
        // corrections workflow
        .route("/admin/corrections", get(corrections::list_corrections))
        .route(
            "/admin/corrections/:id",
            get(corrections::get_correction).put(corrections::update_correction),
        )
        .route(
            "/admin/corrections/:id/publish",
            get(corrections::publish_corrections),
        )
        .route("/admin/correctionInfos", get(corrections::list_correction_infos))
        .route(
            "/admin/correctionInfos/:id",
            get(corrections::get_correction_info),
        )
        .route(
            "/admin/correctionsEmail/:id",
            axum::routing::put(corrections::send_corrections_email),
        )
        // diplomas
        .route("/diplomas/:user_id", get(diplomas::list_diplomas))
        .route("/diplomas/:user_id/:year_id/show", get(diplomas::show_diploma))
        // modules
        .route("/modules/:id", get(modules::get_module))
        .route(
            "/modules/:id/submit",
            axum::routing::post(modules::submit_module),
        )
        .route(
            "/modules/:id/submitFiles",
            axum::routing::post(modules::submit_files),
        )
        .route(
            "/runCode/:moduleId/submit",
            axum::routing::post(modules::run_code),
        )
        // misc
        .route("/robots.txt", get(|| async { "User-agent: *\nDisallow:\n" }))
        .route("/csp", get(|| async { axum::Json(serde_json::json!({})) }))
}
