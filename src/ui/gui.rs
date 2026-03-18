use crate::registry::Registry;
use crate::ui::backend::{UiBackend, UiMessage};
use crate::utils::HumanBytes;
use eframe::egui;

/// Entry point for the GUI.
pub fn run(registry: Registry) -> Result<(), eframe::Error> {
    let backend = UiBackend::spawn(registry);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 600.0])
            .with_title("Warp Download Manager"),
        ..Default::default()
    };
    
    eframe::run_native(
        "Warp - Download Accelerator",
        options,
        Box::new(|_cc| {
            Ok(Box::new(WarpApp::new(backend)))
        }),
    )
}

struct WarpApp {
    backend: UiBackend,
    new_url: String,
}

impl WarpApp {
    fn new(backend: UiBackend) -> Self {
        Self {
            backend,
            new_url: String::new(),
        }
    }
}

impl eframe::App for WarpApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Request frequent repaints to smoothly animates the progress bars
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        
        // Define a beautiful dark theme
        let mut style = (*ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        ctx.set_style(style);

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("🚀 Warp Download Manager").color(egui::Color32::from_rgb(0, 200, 255)));
            });
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Add URL:").strong());
                let text_edit = egui::TextEdit::singleline(&mut self.new_url).desired_width(500.0);
                ui.add(text_edit);
                
                if ui.button("Download").clicked() && !self.new_url.is_empty() {
                    let url = self.new_url.trim().to_string();
                    let filename = url.split('/').last().unwrap_or("download.bin")
                        .split('?').next().unwrap_or("download.bin");
                    
                    let path = std::path::PathBuf::from(filename);

                    // Send to backend
                    let _ = self.backend.tx.try_send(UiMessage::Add(url, path));
                    self.new_url.clear();
                }
            });
            ui.add_space(8.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Active Downloads");
            ui.separator();
            ui.add_space(8.0);

            let state = self.backend.state.read().unwrap();
            
            if state.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(egui::RichText::new("No active downloads").color(egui::Color32::DARK_GRAY).size(18.0));
                });
                return;
            }

            // Create a table-like layout
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (id, progress) in state.iter() {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            // Column 1: Info (Path and URL)
                            ui.vertical(|ui| {
                                ui.set_min_width(300.0);
                                ui.label(egui::RichText::new(&progress.target_path).strong().size(15.0));
                                let display_url = if progress.url.len() > 40 {
                                    format!("{}...", &progress.url[..37])
                                } else {
                                    progress.url.clone()
                                };
                                ui.label(egui::RichText::new(display_url).small().color(egui::Color32::DARK_GRAY));
                            });

                            // Column 2: Status & Speed
                            ui.vertical(|ui| {
                                ui.set_min_width(120.0);
                                let status_text = match progress.status {
                                    crate::registry::DownloadStatus::Downloading => "Downloading",
                                    crate::registry::DownloadStatus::Paused => "Paused",
                                    crate::registry::DownloadStatus::Error(_) => "Error",
                                    crate::registry::DownloadStatus::Completed => "Completed",
                                    crate::registry::DownloadStatus::Pending => "Pending",
                                };
                                let color = match progress.status {
                                    crate::registry::DownloadStatus::Downloading => egui::Color32::from_rgb(0, 255, 128),
                                    crate::registry::DownloadStatus::Paused => egui::Color32::YELLOW,
                                    crate::registry::DownloadStatus::Error(_) => egui::Color32::RED,
                                    crate::registry::DownloadStatus::Completed => egui::Color32::LIGHT_BLUE,
                                    crate::registry::DownloadStatus::Pending => egui::Color32::GRAY,
                                };
                                ui.label(egui::RichText::new(status_text).color(color).strong());
                                if matches!(progress.status, crate::registry::DownloadStatus::Downloading) {
                                    ui.label(format!("{}/s", HumanBytes(progress.speed)));
                                } else {
                                    ui.label("-");
                                }
                            });

                            // Column 3: Progress Bar
                            ui.vertical(|ui| {
                                ui.set_min_width(220.0);
                                let fraction = if progress.total > 0 {
                                    progress.downloaded as f32 / progress.total as f32
                                } else if progress.status == crate::registry::DownloadStatus::Completed {
                                    1.0
                                } else {
                                    0.0
                                };
                                
                                let text = if progress.total > 0 {
                                    format!("{} / {}", HumanBytes(progress.downloaded), HumanBytes(progress.total))
                                } else if progress.status == crate::registry::DownloadStatus::Completed {
                                    format!("{} (Done)", HumanBytes(progress.downloaded))
                                } else {
                                    format!("{} / ?", HumanBytes(progress.downloaded))
                                };
                                
                                ui.add(egui::ProgressBar::new(fraction).text(text).animate(progress.speed > 0));
                                
                                if let crate::registry::DownloadStatus::Error(ref msg) = progress.status {
                                    ui.label(egui::RichText::new(format!("Err: {}", msg)).small().color(egui::Color32::RED));
                                }
                            });
                            
                            // Column 4: Controls
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    let is_active = matches!(progress.status, crate::registry::DownloadStatus::Downloading | crate::registry::DownloadStatus::Pending);
                                    let is_resumable = matches!(progress.status, crate::registry::DownloadStatus::Paused | crate::registry::DownloadStatus::Error(_));
                                    
                                    if is_active {
                                        if ui.button("⏸ Pause").clicked() {
                                            let _ = self.backend.tx.try_send(UiMessage::Pause(id.clone()));
                                        }
                                    } else if is_resumable {
                                        if ui.button("▶ Resume").clicked() {
                                            let _ = self.backend.tx.try_send(UiMessage::Resume(id.clone()));
                                        }
                                    }

                                    if ui.button("❌ Remove").clicked() {
                                        let _ = self.backend.tx.try_send(UiMessage::Remove(id.clone()));
                                    }
                                });
                            });
                        });
                    });
                    ui.add_space(4.0);
                }
            });
        });
    }
}
