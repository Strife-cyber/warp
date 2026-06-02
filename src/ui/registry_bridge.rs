//! Background tokio thread for registry I/O — shared by GUI and TUI without nesting runtimes.

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use crate::core::{AppSettings, DownloadCategory, DownloadEntry, DownloadStatus};
use crate::download_registry::Registry;

enum Request {
    ListFiltered {
        category: Option<DownloadCategory>,
        search: String,
        reply: mpsc::Sender<Vec<DownloadEntry>>,
    },
    GetSettings {
        reply: mpsc::Sender<AppSettings>,
    },
    Add {
        url: String,
        path: PathBuf,
        reply: mpsc::Sender<anyhow::Result<String>>,
    },
    Remove {
        id: String,
        reply: mpsc::Sender<anyhow::Result<()>>,
    },
    Pause {
        id: String,
        reply: mpsc::Sender<anyhow::Result<()>>,
    },
    Resume {
        id: String,
        reply: mpsc::Sender<anyhow::Result<()>>,
    },
    Retry {
        id: String,
        reply: mpsc::Sender<anyhow::Result<()>>,
    },
    Clean {
        reply: mpsc::Sender<anyhow::Result<usize>>,
    },
    RunAll {
        reply: mpsc::Sender<anyhow::Result<()>>,
    },
}

pub struct RegistryBridge {
    tx: mpsc::Sender<Request>,
    _worker: JoinHandle<()>,
}

impl RegistryBridge {
    pub fn new(registry: Registry) -> Self {
        let (tx, rx) = mpsc::channel();

        let worker = thread::Builder::new()
            .name("warp-ui-io".into())
            .spawn(move || {
                let rt = tokio::runtime::Runtime::new().expect("ui io runtime");
                rt.block_on(async move {
                    while let Ok(req) = rx.recv() {
                        match req {
                            Request::ListFiltered {
                                category,
                                search,
                                reply,
                            } => {
                                let search_ref = if search.is_empty() {
                                    None
                                } else {
                                    Some(search.as_str())
                                };
                                let rows = registry
                                    .list_filtered(category, search_ref)
                                    .await
                                    .unwrap_or_default();
                                let _ = reply.send(rows);
                            }
                            Request::GetSettings { reply } => {
                                let settings = registry.get_settings().await.unwrap_or_default();
                                let _ = reply.send(settings);
                            }
                            Request::Add { url, path, reply } => {
                                let result = registry.add(url, path).await;
                                let _ = reply.send(result);
                            }
                            Request::Remove { id, reply } => {
                                let result = registry
                                    .remove(&id)
                                    .await
                                    .map(|_| ())
                                    .map_err(|e| e.into());
                                let _ = reply.send(result);
                            }
                            Request::Pause { id, reply } => {
                                let result = registry
                                    .update_status(&id, DownloadStatus::Paused)
                                    .await
                                    .map_err(|e| e.into());
                                let _ = reply.send(result);
                            }
                            Request::Resume { id, reply } => {
                                let result = registry
                                    .update_status(&id, DownloadStatus::Pending)
                                    .await
                                    .map_err(|e| e.into());
                                let _ = reply.send(result);
                            }
                            Request::Retry { id, reply } => {
                                let result = registry
                                    .update_status(&id, DownloadStatus::Pending)
                                    .await
                                    .map_err(|e| e.into());
                                let _ = reply.send(result);
                            }
                            Request::Clean { reply } => {
                                let result = registry.clean_completed().await.map_err(|e| e.into());
                                let _ = reply.send(result);
                            }
                            Request::RunAll { reply } => {
                                let result = crate::pipeline::run_all(&registry).await;
                                let _ = reply.send(result);
                            }
                        }
                    }
                });
            })
            .expect("spawn ui io thread");

        Self {
            tx,
            _worker: worker,
        }
    }

    pub fn list_filtered(
        &self,
        category: Option<DownloadCategory>,
        search: String,
    ) -> mpsc::Receiver<Vec<DownloadEntry>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.tx.send(Request::ListFiltered {
            category,
            search,
            reply: reply_tx,
        });
        reply_rx
    }

    pub fn get_settings(&self) -> mpsc::Receiver<AppSettings> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.tx.send(Request::GetSettings { reply: reply_tx });
        reply_rx
    }

    pub fn add(&self, url: String, path: PathBuf) -> mpsc::Receiver<anyhow::Result<String>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.tx.send(Request::Add {
            url,
            path,
            reply: reply_tx,
        });
        reply_rx
    }

    pub fn remove(&self, id: String) -> mpsc::Receiver<anyhow::Result<()>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.tx.send(Request::Remove { id, reply: reply_tx });
        reply_rx
    }

    pub fn pause(&self, id: String) -> mpsc::Receiver<anyhow::Result<()>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.tx.send(Request::Pause { id, reply: reply_tx });
        reply_rx
    }

    pub fn resume(&self, id: String) -> mpsc::Receiver<anyhow::Result<()>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.tx.send(Request::Resume { id, reply: reply_tx });
        reply_rx
    }

    pub fn retry(&self, id: String) -> mpsc::Receiver<anyhow::Result<()>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.tx.send(Request::Retry { id, reply: reply_tx });
        reply_rx
    }

    pub fn clean(&self) -> mpsc::Receiver<anyhow::Result<usize>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.tx.send(Request::Clean { reply: reply_tx });
        reply_rx
    }

    pub fn run_all(&self) -> mpsc::Receiver<anyhow::Result<()>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.tx.send(Request::RunAll { reply: reply_tx });
        reply_rx
    }
}
