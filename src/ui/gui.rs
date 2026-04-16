use crate::downloader::registry::Registry;
use crate::ui::backend::{UiBackend, UiMessage};
use crate::downloader::utils::HumanBytes;
use eframe::egui;

#[derive(PartialEq)]
enum GuiTab {
    Downloads,
    Interceptor,
}

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
    current_tab: GuiTab,
    interceptor_running: bool,
    captured_requests: Vec<crate::interceptor::types::CapturedRequest>,
    show_npcap_warning: bool,
}

impl WarpApp {
    fn new(backend: UiBackend) -> Self {
        #[cfg(feature = "capture")]
        let npcaps_installed = crate::interceptor::npcap_check::check_npcap_installed();
        #[cfg(not(feature = "capture"))]
        let npcaps_installed = false;

        Self {
            backend,
            new_url: String::new(),
            current_tab: GuiTab::Downloads,
            interceptor_running: false,
            captured_requests: Vec::new(),
            show_npcap_warning: !npcaps_installed,
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

        // Show Npcap warning if not installed
        if self.show_npcap_warning {
            egui::Window::new("⚠️ Npcap Required")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::new(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.heading("Network Packet Capture Requires Npcap");
                    ui.add_space(10.0);
                    ui.label("Npcap is a library required for capturing network traffic.");
                    ui.label("It is currently not installed on your system.");
                    ui.add_space(15.0);
                    
                    ui.label("To install Npcap:");
                    ui.label("1. Download from: https://nmap.org/npcap/");
                    ui.label(egui::RichText::new("2. During installation, enable 'WinPcap API-compatible Mode'").color(egui::Color32::YELLOW));
                    ui.label("3. Restart your computer after installation");
                    ui.add_space(15.0);
                    
                    ui.horizontal(|ui| {
                        if ui.button("🌐 Open Download Page").clicked() {
                            let _ = std::process::Command::new("cmd")
                                .args(&["/C", "start", "https://nmap.org/npcap/"])
                                .spawn();
                        }
                        
                        if ui.button("✓ Dismiss").clicked() {
                            self.show_npcap_warning = false;
                        }
                    });
                    
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("Note: You can still use the app in demo mode without Npcap.").small().color(egui::Color32::LIGHT_GRAY));
                });
        }

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("🚀 Warp Download Manager").color(egui::Color32::from_rgb(0, 200, 255)));
            });
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                // Tab buttons
                ui.selectable_value(&mut self.current_tab, GuiTab::Downloads, "Downloads");
                ui.selectable_value(&mut self.current_tab, GuiTab::Interceptor, "Interceptor");
            });
            ui.add_space(8.0);

            // Only show URL input on Downloads tab
            if matches!(self.current_tab, GuiTab::Downloads) {
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
            }
            ui.add_space(8.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            match self.current_tab {
                GuiTab::Downloads => {
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
            egui::ScrollArea::both().show(ui, |ui| {
                for (id, progress) in state.iter() {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            // Column 1: Info (Path and URL)
                            ui.vertical(|ui| {
                                ui.set_min_width(250.0);
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
                                ui.set_min_width(100.0);
                                let status_text = match progress.status {
                                    crate::downloader::registry::DownloadStatus::Downloading => "Downloading",
                                    crate::downloader::registry::DownloadStatus::Paused => "Paused",
                                    crate::downloader::registry::DownloadStatus::Error(_) => "Error",
                                    crate::downloader::registry::DownloadStatus::Completed => "Completed",
                                    crate::downloader::registry::DownloadStatus::Pending => "Pending",
                                };
                                let color = match progress.status {
                                    crate::downloader::registry::DownloadStatus::Downloading => egui::Color32::from_rgb(0, 255, 128),
                                    crate::downloader::registry::DownloadStatus::Paused => egui::Color32::YELLOW,
                                    crate::downloader::registry::DownloadStatus::Error(_) => egui::Color32::RED,
                                    crate::downloader::registry::DownloadStatus::Completed => egui::Color32::LIGHT_BLUE,
                                    crate::downloader::registry::DownloadStatus::Pending => egui::Color32::GRAY,
                                };
                                ui.label(egui::RichText::new(status_text).color(color).strong());
                                if matches!(progress.status, crate::downloader::registry::DownloadStatus::Downloading) {
                                    ui.label(format!("{}/s", HumanBytes(progress.speed)));
                                } else {
                                    ui.label("-");
                                }
                            });

                            // Column 3: Progress Bar
                            ui.vertical(|ui| {
                                ui.set_min_width(200.0);
                                let fraction = if progress.total > 0 {
                                    progress.downloaded as f32 / progress.total as f32
                                } else if progress.status == crate::downloader::registry::DownloadStatus::Completed {
                                    1.0
                                } else {
                                    0.0
                                };
                                
                                let text = if progress.total > 0 {
                                    format!("{} / {}", HumanBytes(progress.downloaded), HumanBytes(progress.total))
                                } else if progress.status == crate::downloader::registry::DownloadStatus::Completed {
                                    format!("{} (Done)", HumanBytes(progress.downloaded))
                                } else {
                                    format!("{} / ?", HumanBytes(progress.downloaded))
                                };
                                
                                ui.add(egui::ProgressBar::new(fraction).text(text).animate(progress.speed > 0));
                                
                                if let crate::downloader::registry::DownloadStatus::Error(ref msg) = progress.status {
                                    ui.label(egui::RichText::new(format!("Err: {}", msg)).small().color(egui::Color32::RED));
                                }
                            });
                            
                            // Column 4: Controls
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    let is_active = matches!(progress.status, crate::downloader::registry::DownloadStatus::Downloading | crate::downloader::registry::DownloadStatus::Pending);
                                    let is_resumable = matches!(progress.status, crate::downloader::registry::DownloadStatus::Paused | crate::downloader::registry::DownloadStatus::Error(_));
                                    
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
                }
                GuiTab::Interceptor => {
                    ui.heading("Network Request Interceptor");
                    ui.separator();
                    ui.add_space(8.0);

                    // Interceptor controls
                    ui.horizontal(|ui| {
                        let status_color = if self.interceptor_running {
                            egui::Color32::from_rgb(0, 255, 128)
                        } else {
                            egui::Color32::RED
                        };
                        let status_text = if self.interceptor_running {
                            "Running"
                        } else {
                            "Stopped"
                        };
                        ui.label(egui::RichText::new(format!("Status: {}", status_text)).color(status_color).strong());

                        ui.add_space(16.0);

                        if self.interceptor_running {
                            if ui.button("⏸ Stop").clicked() {
                                self.interceptor_running = false;
                            }
                        } else {
                            if ui.button("▶ Start").clicked() {
                                #[cfg(feature = "capture")]
                                {
                                    use crate::interceptor::npcap_check;
                                    if npcap_check::check_npcap_installed() {
                                        self.interceptor_running = true;
                                        // Add example request for demo
                                        self.captured_requests = vec![
                                            crate::interceptor::types::CapturedRequest {
                                                id: "1".to_string(),
                                                timestamp: 0,
                                                source_ip: "192.168.1.100".to_string(),
                                                destination_ip: "example.com".to_string(),
                                                source_port: 54321,
                                                destination_port: 443,
                                                protocol: "TCP".to_string(),
                                                method: Some("GET".to_string()),
                                                url: Some("/test".to_string()),
                                                host: Some("example.com".to_string()),
                                                user_agent: None,
                                                content_type: None,
                                                content_length: None,
                                                headers: std::collections::HashMap::new(),
                                                payload_size: 100,
                                            }
                                        ];
                                    } else {
                                        // Show the warning again
                                        self.show_npcap_warning = true;
                                    }
                                }
                                #[cfg(not(feature = "capture"))]
                                {
                                    // Show message about needing capture feature
                                    egui::Window::new("Feature Required")
                                        .collapsible(false)
                                        .resizable(false)
                                        .show(ctx, |ui| {
                                            ui.label("Packet capture requires the 'capture' feature.");
                                            ui.label("Run with: cargo run --features capture -- intercept");
                                            ui.add_space(8.0);
                                            ui.label("For now, use: cargo run -- example");
                                        });
                                }
                            }
                        }

                        ui.add_space(8.0);

                        if ui.button("🗑 Clear").clicked() {
                            self.captured_requests.clear();
                        }
                    });

                    ui.add_space(16.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.heading(format!("Captured Requests ({})", self.captured_requests.len()));
                    ui.add_space(8.0);

                    if self.captured_requests.is_empty() {
                        ui.centered_and_justified(|ui| {
                            ui.label(egui::RichText::new("No captured requests").color(egui::Color32::DARK_GRAY).size(18.0));
                        });
                        return;
                    }

                    // Captured requests table
                    egui::ScrollArea::both().show(ui, |ui| {
                        for (i, req) in self.captured_requests.iter().enumerate() {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(format!("#{}", i));
                                    ui.add_space(16.0);
                                    
                                    ui.vertical(|ui| {
                                        let method = req.method.as_deref().unwrap_or("-");
                                        let url = req.url.as_deref().unwrap_or("-");
                                        ui.label(egui::RichText::new(format!("{} {}", method, url)).strong());
                                        ui.label(format!("{}:{} -> {}:{}", 
                                            req.source_ip, req.source_port,
                                            req.destination_ip, req.destination_port
                                        ));
                                    });
                                });
                            });
                            ui.add_space(4.0);
                        }
                    });
                }
            }
        });
    }
}
