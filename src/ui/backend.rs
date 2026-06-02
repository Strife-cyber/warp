use std::sync::{Arc, RwLock};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::{mpsc, Semaphore};
use crate::download_registry::Registry;
use crate::registry::DownloadStatus;
use crate::manager::Manager;
use tokio::task::JoinSet;

#[derive(Clone, Debug)]
pub struct DownloadProgress {
    pub id: String,
    pub url: String,
    pub target_path: String,
    pub status: DownloadStatus,
    pub downloaded: u64,
    pub total: u64,
    pub speed: u64,
}

pub enum UiMessage {
    Add(String, PathBuf),
    Pause(String),
    Resume(String),
    Remove(String),
    Quit,
}

pub struct UiBackend {
    pub state: Arc<RwLock<HashMap<String, DownloadProgress>>>,
    pub tx: mpsc::Sender<UiMessage>,
}

impl UiBackend {
    pub fn spawn(registry: Registry) -> Self {
        let (tx, mut rx) = mpsc::channel::<UiMessage>(32);
        let state = Arc::new(RwLock::new(HashMap::new()));
        let registry = Arc::new(registry);
        
        let state_clone = Arc::clone(&state);
        let registry_init = Arc::clone(&registry);
        
        tokio::spawn(async move {
            if let Ok(entries) = registry_init.list().await {
                let mut s = state_clone.write().unwrap();
                for entry in entries {
                    let downloaded = if entry.status == DownloadStatus::Completed {
                        1
                    } else {
                        0
                    };

                    s.insert(
                        entry.id.clone(),
                        DownloadProgress {
                            id: entry.id.clone(),
                            url: entry.url.clone(),
                            target_path: entry.target_path.to_string_lossy().into_owned(),
                            status: entry.status.clone(),
                            downloaded,
                            total: if entry.status == DownloadStatus::Completed {
                                1
                            } else {
                                0
                            },
                            speed: 0,
                        },
                    );
                }
            }
        });

        let state_clone = Arc::clone(&state);
        let registry_task = Arc::clone(&registry);

        let tx_task = tx.clone();
        tokio::spawn(async move {
            let worker_limit = crate::resources::calculate_optimal_workers().suggested_workers;
            let semaphore = Arc::new(Semaphore::new(worker_limit));
            
            let mut active_downloads = JoinSet::new();
            let mut tokens: HashMap<String, tokio_util::sync::CancellationToken> = HashMap::new();

            loop {
                tokio::select! {
                    Some(msg) = rx.recv() => {
                        match msg {
                            UiMessage::Add(url, path) => {
                                let id = match registry_task.add(url.clone(), path.clone()).await {
                                    Ok(id) => id,
                                    Err(e) => {
                                        eprintln!("Failed to add download: {e}");
                                        continue;
                                    }
                                };
                                {
                                    let mut s = state_clone.write().unwrap();
                                    s.insert(id.clone(), DownloadProgress {
                                        id: id.clone(),
                                        url: url.clone(),
                                        target_path: path.to_string_lossy().into_owned(),
                                        status: DownloadStatus::Pending,
                                        downloaded: 0,
                                        total: 0,
                                        speed: 0,
                                    });
                                }
                                tx_task.send(UiMessage::Resume(id)).await.ok();
                            }
                            UiMessage::Pause(id) => {
                                registry_task.update_status(&id, DownloadStatus::Paused).await.ok();
                                if let Some(token) = tokens.remove(&id) {
                                    token.cancel();
                                }
                                if let Some(p) = state_clone.write().unwrap().get_mut(&id) {
                                    p.status = DownloadStatus::Paused;
                                    p.speed = 0;
                                }
                            }
                            UiMessage::Resume(id) => {
                                let entry_clone = match registry_task.get(&id).await {
                                    Ok(Some(mut entry)) => {
                                        entry.status = DownloadStatus::Downloading;
                                        if registry_task.update_entry(&id, entry.clone()).await.is_err() {
                                            continue;
                                        }
                                        entry
                                    }
                                    _ => continue,
                                };

                                if let Some(p) = state_clone.write().unwrap().get_mut(&id) {
                                    p.status = DownloadStatus::Downloading;
                                }

                                let id_clone = id.clone();
                                let sem_clone = Arc::clone(&semaphore);
                                let state_for_task = Arc::clone(&state_clone);

                                active_downloads.spawn(async move {
                                    let result = Manager::from_entry(&entry_clone).await;
                                    match result {
                                        Ok(mut manager) => {
                                            let meta = Arc::clone(&manager.metadata);
                                            let size = meta.size;
                                            let task_token = manager.cancel_token.clone();
                                            
                                            {
                                                if let Some(p) = state_for_task.write().unwrap().get_mut(&id_clone) {
                                                    p.total = size;
                                                }
                                            }

                                            let poller_token = task_token.clone();
                                            let poller_meta = Arc::clone(&meta);
                                            let poller_state = Arc::clone(&state_for_task);
                                            let poller_id = id_clone.clone();
                                            tokio::spawn(async move {
                                                let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(500));
                                                let mut last_prog = poller_meta.total_progress().await;
                                                loop {
                                                    tokio::select! {
                                                        _ = interval.tick() => {
                                                            let prog = poller_meta.total_progress().await;
                                                            if let Some(p) = poller_state.write().unwrap().get_mut(&poller_id) {
                                                                let delta = prog.saturating_sub(last_prog);
                                                                p.speed = delta * 2;
                                                                p.downloaded = prog;
                                                            }
                                                            last_prog = prog;
                                                        }
                                                        _ = poller_token.cancelled() => break,
                                                    }
                                                }
                                            });

                                            let res: Result<(), anyhow::Error> = manager.run(worker_limit, sem_clone).await;
                                            (id_clone, task_token, res.map(|_| DownloadStatus::Completed).map_err(|e| e.to_string()))
                                        }
                                        Err(e) => (id_clone, tokio_util::sync::CancellationToken::new(), Err(e.to_string())),
                                    }
                                });
                            }
                            UiMessage::Remove(id) => {
                                if let Some(token) = tokens.remove(&id) {
                                    token.cancel();
                                }
                                registry_task.remove(&id).await.ok();
                                state_clone.write().unwrap().remove(&id);
                            }
                            UiMessage::Quit => {
                                for (_, token) in tokens.drain() {
                                    token.cancel();
                                }
                                break;
                            }
                        }
                    }
                    Some(res) = active_downloads.join_next(), if !active_downloads.is_empty() => {
                        match res {
                            Ok((id, _task_token, Ok(status))) => {
                                tokens.remove(&id);
                                registry_task.update_status(&id, status.clone()).await.ok();
                                if let Some(p) = state_clone.write().unwrap().get_mut(&id) {
                                    p.status = status;
                                    p.speed = 0;
                                }
                            }
                            Ok((id, _task_token, Err(_))) => {
                                tokens.remove(&id);
                                registry_task.update_status(&id, DownloadStatus::Error).await.ok();
                                if let Some(p) = state_clone.write().unwrap().get_mut(&id) {
                                    p.status = DownloadStatus::Error;
                                    p.speed = 0;
                                }
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
        });

        Self {
            state,
            tx,
        }
    }
}
