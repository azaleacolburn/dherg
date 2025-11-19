use anyhow::Result;
use axum::Router;
use axum::{
    Extension,
    extract::{Path, State},
    routing::get,
};
use chrono::{DateTime, Utc};
use octocrab::models::repos::RepoCommit;
use octocrab::{Octocrab, Page};
use std::{path::PathBuf, sync::Arc};

const REBUILD_THRESHOLD: u8 = 10;

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
async fn main() -> Result<()> {
    let queue = Arc::new(TaskQueue::default());
    let last_nixpkgs_commit = Arc::new(0);
    let app = Router::new()
        .route("/get_build", get(send_build))
        .layer(Extension(queue));

    tokio::spawn(async move {
        match std::fs::create_dir("input") {
            Ok(_) => {}
            Err(err) => return err.into(),
        };
        let last_polled = chrono::offset::Utc::now();
        let octocrab = octocrab::instance();

        let err = loop {
            let commits = match poll_nix_update(last_polled, &octocrab).await {
                Ok(c) => c,
                Err(err) => break err,
            };
            let commit_count = commits.len();

            if commit_count > REBUILD_THRESHOLD.into() {
                let source_files = match octocrab
                    .repos("NixOS", "nixpkgs")
                    .get_content()
                    .path("/")
                    .r#ref("main")
                    .send()
                    .await
                {
                    Ok(c) => c,
                    Err(err) => break err.into(),
                };

                source_files.items.iter().for_each(|item| {
                    let content = item.decoded_content().unwrap();
                });

                queue.pending.push();
            }
        };

        return err;
    });

    Ok(())
}

async fn send_build(Path(system): Path<String>, State(queue): State<Arc<TaskQueue>>) {}

async fn poll_nix_update(
    last_polled: DateTime<Utc>,
    octocrab: &Octocrab,
) -> Result<Vec<RepoCommit>> {
    let commits = octocrab
        .repos("NixOS", "nixpkgs")
        .list_commits()
        .since(last_polled)
        .send()
        .await?;

    return Ok(commits.items);
}
