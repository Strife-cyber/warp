use crate::downloader::registry::Registry;
use crate::ui::backend::{UiBackend, UiMessage};
use crate::downloader::utils::HumanBytes;
use anyhow::Result;
use std::io;
use std::path::PathBuf;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
    Terminal,
};

enum InputMode {
    Normal,
    Editing,
}

enum Tab {
    Downloads,
    Interceptor,
}

struct App {
    input: String,
    input_mode: InputMode,
    backend: UiBackend,
    table_state: TableState,
    current_tab: Tab,
    interceptor_requests: Vec<crate::interceptor::types::CapturedRequest>,
    interceptor_running: bool,
    show_npcap_warning: bool,
}

impl App {
    fn new(backend: UiBackend) -> Self {
        #[cfg(feature = "capture")]
        let npcaps_installed = crate::interceptor::npcap_check::check_npcap_installed();
        #[cfg(not(feature = "capture"))]
        let npcaps_installed = false;

        Self {
            input: String::new(),
            input_mode: InputMode::Normal,
            backend,
            table_state: TableState::default(),
            current_tab: Tab::Downloads,
            interceptor_requests: Vec::new(),
            interceptor_running: false,
            show_npcap_warning: !npcaps_installed,
        }
    }

    fn switch_tab(&mut self) {
        self.current_tab = match self.current_tab {
            Tab::Downloads => Tab::Interceptor,
            Tab::Interceptor => Tab::Downloads,
        };
        self.table_state = TableState::default();
    }

    fn next(&mut self, item_count: usize) {
        if item_count == 0 {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= item_count - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    fn previous(&mut self, item_count: usize) {
        if item_count == 0 {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    item_count - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }
}

fn make_progress_bar(fraction: f32, width: usize) -> String {
    if width < 2 { return "".to_string(); }
    let fill_width = width - 2;
    let filled = (fraction * fill_width as f32).round() as usize;
    let empty = fill_width.saturating_sub(filled);
    
    let mut bar = String::from("[");
    for _ in 0..filled { bar.push('='); }
    for _ in 0..empty { bar.push(' '); }
    bar.push(']');
    bar
}

/// Entry point for the TUI.
pub fn run(registry: Registry) -> Result<()> {
    let backend = UiBackend::spawn(registry);
    
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend_trm = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend_trm)?;

    let app = App::new(backend);
    let res = run_app(&mut terminal, app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err)
    }

    Ok(())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app: App) -> io::Result<()> {
    loop {
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints(
                    [
                        Constraint::Length(3), // Help/Title
                        Constraint::Min(5),    // Table
                        Constraint::Length(3), // Input
                    ]
                    .as_ref(),
                )
                .split(f.area());

            let title_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
            
            let help_msg = match app.input_mode {
                InputMode::Normal => match app.current_tab {
                    Tab::Downloads => "q: Quit | Tab: Switch View | a: Add | p: Pause | r: Resume | d: Delete | Up/Down: Select",
                    Tab::Interceptor => if app.show_npcap_warning {
                        "q: Quit | Tab: Switch View | d: Dismiss Warning"
                    } else {
                        "q: Quit | Tab: Switch View | s: Start | t: Stop | c: Clear | Up/Down: Select"
                    },
                },
                InputMode::Editing => "Editing Mode: Enter to submit, Esc to cancel",
            };

            let help_block = Block::default()
                .title(Span::styled(" Warp TUI ", title_style))
                .borders(Borders::ALL);
            let help_para = Paragraph::new(help_msg).block(help_block);
            f.render_widget(help_para, chunks[0]);

            // Render based on current tab
            match app.current_tab {
                Tab::Downloads => {
                    let state = app.backend.state.read().unwrap();
                    let mut items: Vec<(String, crate::ui::backend::DownloadProgress)> = state.iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    items.sort_by(|a, b| a.1.target_path.cmp(&b.1.target_path));

                    let mut rows = Vec::new();
                    for (_, progress) in &items {
                        let status_str = match progress.status {
                            crate::downloader::registry::DownloadStatus::Downloading => "Downloading",
                            crate::downloader::registry::DownloadStatus::Paused => "Paused",
                            crate::downloader::registry::DownloadStatus::Error(_) => "Error",
                            crate::downloader::registry::DownloadStatus::Completed => "Completed",
                            crate::downloader::registry::DownloadStatus::Pending => "Pending",
                        };
                        let status_color = match progress.status {
                            crate::downloader::registry::DownloadStatus::Downloading => Color::Green,
                            crate::downloader::registry::DownloadStatus::Paused => Color::Yellow,
                            crate::downloader::registry::DownloadStatus::Error(_) => Color::Red,
                            crate::downloader::registry::DownloadStatus::Completed => Color::LightBlue,
                            crate::downloader::registry::DownloadStatus::Pending => Color::DarkGray,
                        };
                        
                        let speed_str = if progress.status == crate::downloader::registry::DownloadStatus::Downloading {
                            format!("{}/s", HumanBytes(progress.speed))
                        } else {
                            "-".to_string()
                        };

                        let frac = if progress.total > 0 {
                            progress.downloaded as f32 / progress.total as f32
                        } else if progress.status == crate::downloader::registry::DownloadStatus::Completed {
                            1.0
                        } else {
                            0.0
                        };
                        let pb_str = make_progress_bar(frac, 20);
                        
                        let size_str = format!("{}/{}", HumanBytes(progress.downloaded), HumanBytes(progress.total));

                        let cells = vec![
                            Cell::from(progress.target_path.clone()),
                            Cell::from(Span::styled(status_str, Style::default().fg(status_color))),
                            Cell::from(pb_str),
                            Cell::from(size_str),
                            Cell::from(speed_str),
                        ];
                        rows.push(Row::new(cells).height(1).bottom_margin(0));
                    }

                    let table_block = Block::default().title(" Downloads ").borders(Borders::ALL);
                    let header = Row::new(vec!["File", "Status", "Progress", "Size", "Speed"])
                        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
                        .bottom_margin(1);

                    let table = Table::new(rows, [
                        Constraint::Percentage(30),
                        Constraint::Length(12),
                        Constraint::Length(22),
                        Constraint::Length(20),
                        Constraint::Min(10),
                    ])
                    .header(header)
                    .block(table_block)
                    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                    .highlight_symbol(">> ");

                    f.render_stateful_widget(table, chunks[1], &mut app.table_state);
                    drop(state);

                    let input_title = match app.input_mode {
                        InputMode::Normal => " (Idle) ",
                        InputMode::Editing => " Add URL ",
                    };
                    
                    let input_style = match app.input_mode {
                        InputMode::Normal => Style::default(),
                        InputMode::Editing => Style::default().fg(Color::Yellow),
                    };

                    let input = Paragraph::new(app.input.as_str())
                        .style(input_style)
                        .block(Block::default().borders(Borders::ALL).title(input_title));
                    f.render_widget(input, chunks[2]);

                    if let InputMode::Editing = app.input_mode {
                        f.set_cursor_position((
                            chunks[2].x + app.input.len() as u16 + 1,
                            chunks[2].y + 1,
                        ));
                    }
                }
                Tab::Interceptor => {
                    // Render interceptor view
                    let status_text = if app.interceptor_running {
                        "Status: Running".to_string()
                    } else {
                        "Status: Stopped".to_string()
                    };
                    let status_color = if app.interceptor_running { Color::Green } else { Color::Red };

                    let status_block = Block::default()
                        .title(" Interceptor Status ")
                        .borders(Borders::ALL);
                    let status_para = Paragraph::new(Span::styled(status_text, Style::default().fg(status_color)))
                        .block(status_block);
                    f.render_widget(status_para, chunks[1]);

                    // Show Npcap warning if not installed
                    if app.show_npcap_warning {
                        let warning_block = Block::default()
                            .title(" ⚠️  Npcap Required ")
                            .borders(Borders::ALL);
                        let warning_text = vec![
                            ratatui::prelude::Line::from(Span::styled("Npcap is required for network packet capture.", Style::default().fg(Color::Yellow))),
                            ratatui::prelude::Line::from("Install from: https://nmap.org/npcap/"),
                            ratatui::prelude::Line::from(Span::styled("Enable 'WinPcap API-compatible Mode' during installation.", Style::default().fg(Color::Yellow))),
                            ratatui::prelude::Line::from("Press 'd' to dismiss this warning."),
                        ];
                        let warning_para = Paragraph::new(warning_text).block(warning_block);
                        f.render_widget(warning_para, chunks[2]);
                        return; // Don't show the table when warning is active
                    }

                    // Render captured requests table
                    let mut rows = Vec::new();
                    for (i, req) in app.interceptor_requests.iter().enumerate() {
                        let method = req.method.as_deref().unwrap_or("-");
                        let url = req.url.as_deref().unwrap_or("-");
                        let url_short = if url.len() > 50 {
                            format!("{}...", &url[..47])
                        } else {
                            url.to_string()
                        };
                        
                        let cells = vec![
                            Cell::from(i.to_string()),
                            Cell::from(method),
                            Cell::from(req.source_ip.clone()),
                            Cell::from(req.destination_ip.clone()),
                            Cell::from(url_short),
                        ];
                        rows.push(Row::new(cells).height(1).bottom_margin(0));
                    }

                    let table_block = Block::default().title(" Captured Requests ").borders(Borders::ALL);
                    let header = Row::new(vec!["#", "Method", "Source", "Dest", "URL"])
                        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
                        .bottom_margin(1);

                    let table = Table::new(rows, [
                        Constraint::Length(5),
                        Constraint::Length(8),
                        Constraint::Length(20),
                        Constraint::Length(20),
                        Constraint::Min(30),
                    ])
                    .header(header)
                    .block(table_block)
                    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                    .highlight_symbol(">> ");

                    f.render_stateful_widget(table, chunks[2], &mut app.table_state);
                }
            }
        })?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == event::KeyEventKind::Press {
                    let state = app.backend.state.read().unwrap();
                    let mut items: Vec<String> = state.keys().cloned().collect();
                    items.sort();
                    let item_count = items.len();
                    
                    let selected_id = if let Some(idx) = app.table_state.selected() {
                        if idx < items.len() {
                            Some(items[idx].clone())
                        } else { None }
                    } else { None };
                    drop(state);

                    match app.input_mode {
                        InputMode::Normal => match app.current_tab {
                            Tab::Downloads => match key.code {
                                KeyCode::Char('q') => {
                                    let _ = app.backend.tx.try_send(UiMessage::Quit);
                                    return Ok(());
                                }
                                KeyCode::Tab => {
                                    app.switch_tab();
                                }
                                KeyCode::Char('a') => {
                                    app.input_mode = InputMode::Editing;
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    app.next(item_count);
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    app.previous(item_count);
                                }
                                KeyCode::Char('p') => {
                                    if let Some(id) = selected_id {
                                        let _ = app.backend.tx.try_send(UiMessage::Pause(id));
                                    }
                                }
                                KeyCode::Char('r') => {
                                    if let Some(id) = selected_id {
                                        let _ = app.backend.tx.try_send(UiMessage::Resume(id));
                                    }
                                }
                                KeyCode::Char('d') => {
                                    if let Some(id) = selected_id {
                                        let _ = app.backend.tx.try_send(UiMessage::Remove(id));
                                        // adjust selection
                                        if let Some(idx) = app.table_state.selected() {
                                            if idx >= item_count.saturating_sub(1) && idx > 0 {
                                                app.table_state.select(Some(idx - 1));
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            },
                            Tab::Interceptor => match key.code {
                                KeyCode::Char('q') => {
                                    let _ = app.backend.tx.try_send(UiMessage::Quit);
                                    return Ok(());
                                }
                                KeyCode::Tab => {
                                    app.switch_tab();
                                }
                                KeyCode::Char('d') => {
                                    app.show_npcap_warning = false;
                                }
                                KeyCode::Char('s') => {
                                    // Start interceptor (placeholder - requires capture feature)
                                    #[cfg(feature = "capture")]
                                    {
                                        use crate::interceptor::npcap_check;
                                        if npcap_check::check_npcap_installed() {
                                            app.interceptor_running = true;
                                            // Add some example requests for demo
                                            app.interceptor_requests = vec![
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
                                            // Npcap not installed - show message via print (TUI limitation)
                                            // In a real implementation, this would show in the TUI
                                            app.show_npcap_warning = true;
                                        }
                                    }
                                    #[cfg(not(feature = "capture"))]
                                    {
                                        // Show message about needing capture feature
                                    }
                                }
                                KeyCode::Char('t') => {
                                    app.interceptor_running = false;
                                    app.interceptor_requests.clear();
                                }
                                KeyCode::Char('c') => {
                                    app.interceptor_requests.clear();
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    app.next(app.interceptor_requests.len());
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    app.previous(app.interceptor_requests.len());
                                }
                                _ => {}
                            },
                        },
                    InputMode::Editing => match key.code {
                        KeyCode::Enter => {
                            if !app.input.is_empty() {
                                let url = app.input.clone();
                                let filename = url.split('/').last().unwrap_or("download.bin")
                                    .split('?').next().unwrap_or("download.bin").to_string();
                                
                                let _ = app.backend.tx.try_send(UiMessage::Add(url, PathBuf::from(filename)));
                                app.input.clear();
                            }
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Char(c) => {
                            app.input.push(c);
                        }
                        KeyCode::Backspace => {
                            app.input.pop();
                        }
                        KeyCode::Esc => {
                            app.input_mode = InputMode::Normal;
                        }
                        _ => {}
                    },
                }
            }
        }
    }
}
}

