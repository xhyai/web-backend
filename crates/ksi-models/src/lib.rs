pub mod article;
pub mod evaluation;
pub mod module;
pub mod post;
pub mod task;
pub mod thread;
pub mod user;
pub mod wave;
pub mod year;

pub use article::{ArticleJson, ArticleRow};
pub use evaluation::Evaluation;
pub use module::{Module, ModuleJson};
pub use post::{Post, PostJson};
pub use task::{Task, TaskJson};
pub use thread::{Thread, ThreadJson};
pub use user::{User, UserJson};
pub use wave::{Wave, WaveJson};
pub use year::{Year, YearJson};

pub(crate) fn naive_to_iso(dt: &Option<chrono::NaiveDateTime>) -> Option<String> {
    dt.map(|d| d.format("%Y-%m-%dT%H:%M:%S%.6f").to_string())
}
