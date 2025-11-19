use axum::{
    Extension,
    extract::{Path, State},
    routing::get,
};
use std::{path::PathBuf, sync::Arc};

use axum::Router;

struct Task {
    input: PathBuf,
    output: PathBuf,
}

#[derive(Default)]
struct TaskQueue {
    pending: Vec<Task>,
    in_progress: Vec<Task>,
    complete: Vec<Task>,
}

#[tokio::main]
async fn main() {
    let queue = Arc::new(TaskQueue::default());
    let app = Router::new()
        .route("/get_build", get(get_build))
        .layer(Extension(queue));
}

async fn get_build(Path(system): Path<String>, State(queue): State<Arc<TaskQueue>>) {}
