//! Beautiful dark-themed download manager (categories + search + full CRUD).

use std::path::PathBuf;

use eframe::egui;
use egui::{Color32, RichText};

use crate::core::{DownloadCategory, DownloadStatus};
use crate::download_registry::Registry;
use crate::ui::RegistryBridge;

pub fn run_gui(registry: Registry) -> anyhow::Result<()> {
    let bridge = RegistryBridge::new(registry);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_title("Warp Download Manager"),
        ..Default::default()
    };

    eframe::run_native(
        "Warp",
        native_options,
        Box::new(move |cc| {
            setup_theme(&cc.egui_ctx);
            Ok(Box::new(WarpApp {
                bridge,
                selected_category: None,
                search: String::new(),
                add_url: String::new(),
                add_output: String::new(),
                selected_id: None,
                entries: Vec::new(),
                max_workers: 32,
                status_message: String::new(),
                last_refresh: std::time::Instant::now(),
                needs_refresh: true,
                settings_loaded: false,
                pending_list: None,
                pending_run: None,
                pending_settings: None,
                pending_action: None,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

fn setup_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(28, 28, 34);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(38, 38, 48);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(55, 55, 72);
    visuals.widgets.active.bg_fill = Color32::from_rgb(72, 118, 255);
    visuals.selection.bg_fill = Color32::from_rgb(72, 118, 255);
    ctx.set_visuals(visuals);
}

enum PendingAction {
    Add(std::sync::mpsc::Receiver<anyhow::Result<String>>),
    Remove(std::sync::mpsc::Receiver<anyhow::Result<()>>),
    Pause(std::sync::mpsc::Receiver<anyhow::Result<()>>),
    Resume(std::sync::mpsc::Receiver<anyhow::Result<()>>),
    Retry(std::sync::mpsc::Receiver<anyhow::Result<()>>),
    Clean(std::sync::mpsc::Receiver<anyhow::Result<usize>>),
}

struct WarpApp {
    bridge: RegistryBridge,
    selected_category: Option<DownloadCategory>,
    search: String,
    add_url: String,
    add_output: String,
    selected_id: Option<String>,
    entries: Vec<crate::core::DownloadEntry>,
    max_workers: usize,
    status_message: String,
    last_refresh: std::time::Instant,
    needs_refresh: bool,
    settings_loaded: bool,
    pending_list: Option<std::sync::mpsc::Receiver<Vec<crate::core::DownloadEntry>>>,
    pending_run: Option<std::sync::mpsc::Receiver<anyhow::Result<()>>>,
    pending_settings: Option<std::sync::mpsc::Receiver<crate::core::AppSettings>>,
    pending_action: Option<PendingAction>,
}

impl WarpApp {
    fn request_refresh(&mut self) {
        self.pending_list = Some(self.bridge.list_filtered(
            self.selected_category.clone(),
            self.search.clone(),
        ));
    }

    fn poll_pending(&mut self) {
        if let Some(rx) = &self.pending_list {
            if let Ok(rows) = rx.try_recv() {
                self.entries = rows;
                if let Some(sel) = &self.selected_id {
                    if !self.entries.iter().any(|e| &e.id == sel) {
                        self.selected_id = None;
                    }
                }
                self.last_refresh = std::time::Instant::now();
                self.pending_list = None;
            }
        }
        if let Some(rx) = &self.pending_run {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(()) => self.status_message = "Queue finished.".into(),
                    Err(e) => self.status_message = format!("Run failed: {e}"),
                }
                self.needs_refresh = true;
                self.pending_run = None;
            }
        }
        if let Some(rx) = &mut self.pending_settings {
            if let Ok(settings) = rx.try_recv() {
                self.max_workers = settings.max_workers;
                self.settings_loaded = true;
                self.pending_settings = None;
            }
        }
        if let Some(action) = &self.pending_action {
            let done = match action {
                PendingAction::Add(rx) => rx.try_recv().ok().map(|r| {
                    self.status_message = match r {
                        Ok(id) => format!("Added download {id}"),
                        Err(e) => format!("Add failed: {e}"),
                    };
                }),
                PendingAction::Remove(rx) => rx.try_recv().ok().map(|r| {
                    self.status_message = match r {
                        Ok(()) => "Removed download.".into(),
                        Err(e) => format!("Remove failed: {e}"),
                    };
                    self.selected_id = None;
                }),
                PendingAction::Pause(rx) | PendingAction::Resume(rx) | PendingAction::Retry(rx) => {
                    rx.try_recv().ok().map(|r| {
                        self.status_message = match r {
                            Ok(()) => "Updated.".into(),
                            Err(e) => format!("Action failed: {e}"),
                        };
                    })
                }
                PendingAction::Clean(rx) => rx.try_recv().ok().map(|r| {
                    self.status_message = match r {
                        Ok(n) => format!("Cleaned {n} completed download(s)."),
                        Err(e) => format!("Clean failed: {e}"),
                    };
                }),
            };
            if done.is_some() {
                self.pending_action = None;
                self.needs_refresh = true;
            }
        }
    }

    fn add_download(&mut self) {
        if self.add_url.trim().is_empty() {
            self.status_message = "Enter a URL first.".into();
            return;
        }
        let path = if self.add_output.trim().is_empty() {
            let url = self.add_url.trim();
            let filename = url.split('/').next_back().unwrap_or("download.bin");
            let filename = filename.split('?').next().unwrap_or("download.bin");
            PathBuf::from(filename)
        } else {
            PathBuf::from(self.add_output.trim())
        };
        self.pending_action = Some(PendingAction::Add(
            self.bridge.add(self.add_url.trim().to_string(), path),
        ));
        self.add_url.clear();
        self.add_output.clear();
    }
}

impl eframe::App for WarpApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.settings_loaded && self.pending_settings.is_none() {
            self.pending_settings = Some(self.bridge.get_settings());
        }

        self.poll_pending();

        if self.needs_refresh && self.pending_list.is_none() {
            self.request_refresh();
            self.needs_refresh = false;
        } else if self.pending_list.is_none() && self.last_refresh.elapsed().as_secs() >= 2 {
            self.request_refresh();
        }

        egui::SidePanel::left("categories")
            .resizable(false)
            .default_width(180.0)
            .show(ctx, |ui| {
                ui.heading(RichText::new("Warp").color(Color32::from_rgb(120, 170, 255)));
                ui.separator();
                ui.label(RichText::new("Categories").weak());
                if ui.selectable_label(self.selected_category.is_none(), "All").clicked() {
                    self.selected_category = None;
                    self.needs_refresh = true;
                }
                for cat in DownloadCategory::all() {
                    let selected = self.selected_category.as_ref() == Some(cat);
                    if ui.selectable_label(selected, cat.label()).clicked() {
                        self.selected_category = Some(cat.clone());
                        self.needs_refresh = true;
                    }
                }
            });

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Search");
                if ui.text_edit_singleline(&mut self.search).changed() {
                    self.needs_refresh = true;
                }
                if ui.button("Refresh").clicked() {
                    self.needs_refresh = true;
                }
                if ui.button("Run pending").clicked() && self.pending_run.is_none() {
                    self.pending_run = Some(self.bridge.run_all());
                    self.status_message = "Running queue…".into();
                }
                if ui.button("Clean completed").clicked() && self.pending_action.is_none() {
                    self.pending_action = Some(PendingAction::Clean(self.bridge.clean()));
                }
            });
            ui.horizontal(|ui| {
                ui.label("URL");
                ui.add(egui::TextEdit::singleline(&mut self.add_url).desired_width(320.0));
                ui.label("Output");
                ui.add(
                    egui::TextEdit::singleline(&mut self.add_output)
                        .desired_width(160.0)
                        .hint_text("optional"),
                );
                if ui.button("Add").clicked() && self.pending_action.is_none() {
                    self.add_download();
                }
            });
            if !self.status_message.is_empty() {
                ui.label(RichText::new(&self.status_message).weak());
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("downloads")
                    .striped(true)
                    .spacing([12.0, 8.0])
                    .min_col_width(80.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new("").strong());
                        ui.label(RichText::new("ID").strong());
                        ui.label(RichText::new("Status").strong());
                        ui.label(RichText::new("Category").strong());
                        ui.label(RichText::new("Target").strong());
                        ui.label(RichText::new("URL").strong());
                        ui.label(RichText::new("Actions").strong());
                        ui.end_row();

                        for entry in self.entries.clone() {
                            let selected = self.selected_id.as_deref() == Some(entry.id.as_str());
                            let (status_color, status_text) = match entry.status {
                                DownloadStatus::Downloading => (Color32::LIGHT_GREEN, "Downloading"),
                                DownloadStatus::Completed => (Color32::LIGHT_BLUE, "Completed"),
                                DownloadStatus::Paused => (Color32::YELLOW, "Paused"),
                                DownloadStatus::Error => (Color32::LIGHT_RED, "Error"),
                                DownloadStatus::Pending => (Color32::GRAY, "Pending"),
                            };

                            if ui.radio(selected, "").clicked() {
                                self.selected_id = Some(entry.id.clone());
                            }
                            ui.monospace(&entry.id);
                            ui.colored_label(status_color, status_text);
                            ui.label(entry.category.label());
                            ui.label(entry.target_path.to_string_lossy());
                            ui.label(&entry.url);

                            let busy = self.pending_action.is_some();
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(!busy, egui::Button::new("Pause"))
                                    .clicked()
                                {
                                    self.pending_action = Some(PendingAction::Pause(
                                        self.bridge.pause(entry.id.clone()),
                                    ));
                                }
                                if ui
                                    .add_enabled(!busy, egui::Button::new("Resume"))
                                    .clicked()
                                {
                                    self.pending_action = Some(PendingAction::Resume(
                                        self.bridge.resume(entry.id.clone()),
                                    ));
                                }
                                if ui
                                    .add_enabled(!busy, egui::Button::new("Retry"))
                                    .clicked()
                                {
                                    self.pending_action = Some(PendingAction::Retry(
                                        self.bridge.retry(entry.id.clone()),
                                    ));
                                }
                                if ui
                                    .add_enabled(!busy, egui::Button::new("Remove"))
                                    .clicked()
                                {
                                    self.pending_action = Some(PendingAction::Remove(
                                        self.bridge.remove(entry.id.clone()),
                                    ));
                                }
                            });
                            ui.end_row();
                        }
                    });
            });

            if self.entries.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.label(
                        RichText::new("No downloads match your filters")
                            .color(Color32::GRAY)
                            .size(16.0),
                    );
                });
            }
        });

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("{} download(s)", self.entries.len()));
                ui.separator();
                ui.label(format!("Worker cap: {}", self.max_workers));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new("SQLite · auto-refresh 2s · `warp config --max-workers N`")
                            .weak(),
                    );
                });
            });
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}
