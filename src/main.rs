use anyhow::Result;
use axum::{
    Extension,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use axum::{Router, debug_handler};
use chrono::{DateTime, Utc};
use octocrab::Octocrab;
use octocrab::models::repos::RepoCommit;
use std::{fs::File, process::Command, thread::sleep};
use std::{
    io::{Read, Write},
    time::Duration,
};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use walkdir::WalkDir;
use zip::write::{FileOptions, FullFileOptions};

const REBUILD_THRESHOLD: u8 = 10;

#[derive(Debug, PartialEq, Clone)]
struct Task {
    system: String,
    input: PathBuf,
    output: PathBuf,
}

#[derive(Default)]
struct TaskQueue {
    pending: Vec<Task>,
    in_progress: Vec<Task>,
    complete: Vec<Task>,
}

const SYSTEMS: [&str; 3] = ["alurya", "gilarabrywn", "esrahaddon"];

#[tokio::main]
async fn main() -> Result<()> {
    let queue = Arc::new(Mutex::new(TaskQueue::default()));
    let app = Router::new()
        .route("/get_build", get(send_build))
        .layer(Extension(queue.clone()));

    tokio::spawn({
        let queue = queue.clone();
        async move {
            loop {
                sleep(Duration::from_millis(500));
                let task = queue.lock().await.pending.pop();
                match task {
                    Some(task) => {
                        queue.lock().await.in_progress.push(task.clone());
                        Command::new("nixos-rebuild")
                            .arg(format!(
                                "--flake {}#{}",
                                task.input.to_str().unwrap(),
                                task.system
                            ))
                            .spawn()
                            .unwrap();

                        let i = queue
                            .lock()
                            .await
                            .in_progress
                            .iter()
                            .position(|task1| *task1 == task)
                            .unwrap();
                        let mut lock = queue.lock().await;
                        lock.in_progress.remove(i);
                        lock.complete.push(task);
                    }
                    None => continue,
                }
            }
        }
    });

    // Polling for new commits
    let cloned_queue = queue.clone();
    tokio::spawn(async move {
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

                for system in SYSTEMS.iter() {
                    let mut curr_dir = format!("{system}/in");
                    match std::fs::remove_dir_all(system) {
                        Ok(_) => (),
                        Err(err) => panic!("idk: {}", err),
                    }
                    std::fs::create_dir(&curr_dir).unwrap();
                    source_files.items.iter().for_each(|item| {
                        let name = &item.name;
                        match item.decoded_content() {
                            Some(c) => std::fs::write(format!("{curr_dir}/{name}"), c),
                            None => {
                                curr_dir = format!("{}/{name}", curr_dir);
                                std::fs::create_dir(&curr_dir)
                            }
                        }
                        .unwrap();
                    });

                    let task = Task {
                        input: format!("{system}/in").into(),
                        output: format!("{system}/out").into(),
                        system: system.to_string(),
                    };

                    cloned_queue.lock().await.pending.push(task);
                }
            }
        };

        return err;
    });

    Ok(())
}

fn contains<'a>(list: &'a [Task], system: &str) -> Option<&'a Task> {
    list.iter().find(|task| task.system == system)
}

#[debug_handler]
async fn send_build(
    Path(system): Path<String>,
    State(queue): State<Arc<Mutex<TaskQueue>>>,
) -> Result<Vec<u8>, (StatusCode, String)> {
    let queue = queue.lock().await;

    let out = match contains(&queue.complete, &system) {
        Some(task) => &task.output,
        None => match contains(&queue.in_progress, &system) {
            Some(_task) => return Err((StatusCode::NO_CONTENT, "Build In Progress".into())),
            None => match contains(&queue.pending, &system) {
                Some(_task) => return Err((StatusCode::NO_CONTENT, "Build Pending".into())),
                None => return Err((StatusCode::INTERNAL_SERVER_ERROR, "Build lost".into())),
            },
        },
    };
    let out_file = format!("{system}.zip");
    let _ = zip_dir(out.to_str().unwrap(), out_file.as_str());

    let bytes = match std::fs::read_to_string(out_file) {
        Ok(s) => s.into_bytes(),
        Err(err) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read output string: {err}"),
            ));
        }
    };
    Ok(bytes)
}

fn zip_dir(src_dir: &str, dst_file: &str) -> zip::result::ZipResult<()> {
    let path = std::path::Path::new(src_dir);
    let file = File::create(dst_file)?;
    let mut zip = zip::ZipWriter::new(file);

    let options: FullFileOptions = FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o755);

    let mut buffer = Vec::new();

    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        let entry_path = entry.path();
        let name = entry_path.strip_prefix(path).unwrap();

        if entry_path.is_file() {
            // Add file
            zip.start_file(name.to_string_lossy(), options.clone())?;
            let mut f = std::fs::File::open(entry_path)?;
            f.read_to_end(&mut buffer)?;
            zip.write_all(&buffer)?;
            buffer.clear();
        } else if name.as_os_str().len() != 0 {
            // Add directory (Zip needs trailing slash)
            zip.add_directory(name.to_string_lossy() + "/", options.clone())?;
        }
    }

    zip.finish()?;
    Ok(())
}

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
