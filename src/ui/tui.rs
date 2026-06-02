use std::path::PathBuf;

use crate::core::{DownloadCategory, DownloadStatus};
use crate::download_registry::Registry;
use crate::ui::RegistryBridge;
use anyhow::Result;
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
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Tabs},
    Terminal,
};
use std::io;

#[derive(PartialEq)]
enum InputMode {
    Normal,
    AddUrl,
    Search,
}

struct App {
    bridge: RegistryBridge,
    input: String,
    input_mode: InputMode,
    selected_category: Option<DownloadCategory>,
    category_tab: usize,
    search: String,
    entries: Vec<crate::core::DownloadEntry>,
    max_workers: usize,
    status_message: String,
    table_state: TableState,
    last_refresh: std::time::Instant,
    needs_refresh: bool,
    pending_list: Option<std::sync::mpsc::Receiver<Vec<crate::core::DownloadEntry>>>,
    pending_run: Option<std::sync::mpsc::Receiver<anyhow::Result<()>>>,
    pending_settings: Option<std::sync::mpsc::Receiver<crate::core::AppSettings>>,
    pending_action: bool,
}

impl App {
    fn new(bridge: RegistryBridge) -> Self {
        let pending_settings = Some(bridge.get_settings());
        Self {
            bridge,
            input: String::new(),
            input_mode: InputMode::Normal,
            selected_category: None,
            category_tab: 0,
            search: String::new(),
            entries: Vec::new(),
            max_workers: 32,
            status_message: String::new(),
            table_state: TableState::default(),
            last_refresh: std::time::Instant::now(),
            needs_refresh: true,
            pending_list: None,
            pending_run: None,
            pending_settings,
            pending_action: false,
        }
    }

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
                self.last_refresh = std::time::Instant::now();
                self.pending_list = None;
            }
        }
        if let Some(rx) = &self.pending_run {
            if let Ok(result) = rx.try_recv() {
                self.status_message = match result {
                    Ok(()) => "Queue finished.".into(),
                    Err(e) => format!("Run failed: {e}"),
                };
                self.needs_refresh = true;
                self.pending_run = None;
            }
        }
        if let Some(rx) = &mut self.pending_settings {
            if let Ok(settings) = rx.try_recv() {
                self.max_workers = settings.max_workers;
                self.pending_settings = None;
            }
        }
    }

    fn selected_id(&self) -> Option<String> {
        self.table_state
            .selected()
            .and_then(|idx| self.entries.get(idx).map(|e| e.id.clone()))
    }

    fn next(&mut self) {
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) if i + 1 >= n => 0,
            Some(i) => i + 1,
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    fn previous(&mut self) {
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        let i = match self.table_state.selected() {
            Some(0) | None => n - 1,
            Some(i) => i - 1,
        };
        self.table_state.select(Some(i));
    }

    fn category_titles() -> Vec<&'static str> {
        let mut titles = vec!["All"];
        titles.extend(DownloadCategory::all().iter().map(|c| c.label()));
        titles
    }

    fn apply_category_tab(&mut self, tab: usize) {
        self.category_tab = tab;
        self.selected_category = if tab == 0 {
            None
        } else {
            Some(DownloadCategory::all()[tab - 1].clone())
        };
        self.needs_refresh = true;
    }
}

pub fn run(registry: Registry) -> Result<()> {
    let bridge = RegistryBridge::new(registry);
    let app = App::new(bridge);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend_trm = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend_trm)?;

    let res = run_app(&mut terminal, app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{err:?}");
    }

    Ok(())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app: App) -> io::Result<()> {
    loop {
        app.poll_pending();
        if app.needs_refresh && app.pending_list.is_none() {
            app.request_refresh();
            app.needs_refresh = false;
        } else if app.pending_list.is_none() && app.last_refresh.elapsed().as_millis() >= 1500 {
            app.request_refresh();
        }

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Min(5),
                    Constraint::Length(2),
                    Constraint::Length(3),
                ])
                .split(f.area());

            let help_msg = match app.input_mode {
                InputMode::Normal => "q:Quit a:Add /:Search p:Pause r:Resume y:Retry d:Delete c:Clean R:Run g:Category",
                InputMode::AddUrl => "Enter URL · optional path after space · Enter submit · Esc cancel",
                InputMode::Search => "Search filter · Enter apply · Esc cancel",
            };
            let help_block = Block::default()
                .title(Span::styled(" Warp TUI ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
                .borders(Borders::ALL);
            f.render_widget(Paragraph::new(help_msg).block(help_block), chunks[0]);

            let tabs = Tabs::new(App::category_titles())
                .block(Block::default().borders(Borders::ALL).title(" Category "))
                .select(app.category_tab)
                .style(Style::default().fg(Color::White))
                .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
            f.render_widget(tabs, chunks[1]);

            let mut rows = Vec::new();
            for entry in &app.entries {
                let (status_str, status_color) = match entry.status {
                    DownloadStatus::Downloading => ("Downloading", Color::Green),
                    DownloadStatus::Paused => ("Paused", Color::Yellow),
                    DownloadStatus::Error => ("Error", Color::Red),
                    DownloadStatus::Completed => ("Completed", Color::LightBlue),
                    DownloadStatus::Pending => ("Pending", Color::DarkGray),
                };
                rows.push(Row::new(vec![
                    Cell::from(entry.id.clone()),
                    Cell::from(Span::styled(status_str, Style::default().fg(status_color))),
                    Cell::from(entry.category.label()),
                    Cell::from(entry.target_path.to_string_lossy().to_string()),
                    Cell::from(entry.url.clone()),
                ]));
            }

            let table = Table::new(
                rows,
                [
                    Constraint::Length(14),
                    Constraint::Length(12),
                    Constraint::Length(10),
                    Constraint::Percentage(25),
                    Constraint::Min(20),
                ],
            )
            .header(
                Row::new(vec!["ID", "Status", "Category", "Target", "URL"])
                    .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
                    .bottom_margin(1),
            )
            .block(Block::default().title(" Downloads ").borders(Borders::ALL))
            .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol(">> ");

            f.render_stateful_widget(table, chunks[2], &mut app.table_state);

            let footer = if app.status_message.is_empty() {
                format!(
                    "{} download(s) · worker cap {} · search: {}",
                    app.entries.len(),
                    app.max_workers,
                    if app.search.is_empty() { "(none)" } else { &app.search }
                )
            } else {
                app.status_message.clone()
            };

            let input_title = match app.input_mode {
                InputMode::Normal => " (Idle) ",
                InputMode::AddUrl => " Add ",
                InputMode::Search => " Search ",
            };
            let input_style = match app.input_mode {
                InputMode::Normal => Style::default(),
                _ => Style::default().fg(Color::Yellow),
            };
            f.render_widget(
                Paragraph::new(app.input.as_str())
                    .style(input_style)
                    .block(Block::default().borders(Borders::ALL).title(input_title)),
                chunks[3],
            );

            let footer_block = Block::default().borders(Borders::TOP);
            f.render_widget(Paragraph::new(footer).block(footer_block), chunks[4]);

            if app.input_mode != InputMode::Normal {
                f.set_cursor_position((
                    chunks[3].x + app.input.len() as u16 + 1,
                    chunks[3].y + 1,
                ));
            }
        })?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != event::KeyEventKind::Press {
                    continue;
                }

                match app.input_mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('a') => {
                            app.input_mode = InputMode::AddUrl;
                            app.input.clear();
                        }
                        KeyCode::Char('/') => {
                            app.input_mode = InputMode::Search;
                            app.input = app.search.clone();
                        }
                        KeyCode::Char('R') if !app.pending_run.is_some() => {
                            app.pending_run = Some(app.bridge.run_all());
                            app.status_message = "Running queue…".into();
                        }
                        KeyCode::Char('p') if !app.pending_action => {
                            if let Some(id) = app.selected_id() {
                                let _ = app.bridge.pause(id);
                                app.pending_action = true;
                                app.needs_refresh = true;
                                app.status_message = "Paused.".into();
                                app.pending_action = false;
                            }
                        }
                        KeyCode::Char('r') if !app.pending_action => {
                            if let Some(id) = app.selected_id() {
                                let _ = app.bridge.resume(id);
                                app.status_message = "Marked pending.".into();
                                app.needs_refresh = true;
                            }
                        }
                        KeyCode::Char('y') if !app.pending_action => {
                            if let Some(id) = app.selected_id() {
                                let _ = app.bridge.retry(id);
                                app.status_message = "Marked for retry.".into();
                                app.needs_refresh = true;
                            }
                        }
                        KeyCode::Char('d') if !app.pending_action => {
                            if let Some(id) = app.selected_id() {
                                let _ = app.bridge.remove(id);
                                app.status_message = "Removed.".into();
                                app.needs_refresh = true;
                            }
                        }
                        KeyCode::Char('c') if !app.pending_action => {
                            let _ = app.bridge.clean();
                            app.status_message = "Cleaned completed.".into();
                            app.needs_refresh = true;
                        }
                        KeyCode::Down | KeyCode::Char('j') => app.next(),
                        KeyCode::Up | KeyCode::Char('k') => app.previous(),
                        KeyCode::Tab => {
                            let next = (app.category_tab + 1) % App::category_titles().len();
                            app.apply_category_tab(next);
                        }
                        KeyCode::BackTab => {
                            let n = App::category_titles().len();
                            let next = if app.category_tab == 0 {
                                n - 1
                            } else {
                                app.category_tab - 1
                            };
                            app.apply_category_tab(next);
                        }
                        KeyCode::Right => {
                            let next = (app.category_tab + 1) % App::category_titles().len();
                            app.apply_category_tab(next);
                        }
                        KeyCode::Left => {
                            let n = App::category_titles().len();
                            let next = if app.category_tab == 0 {
                                n - 1
                            } else {
                                app.category_tab - 1
                            };
                            app.apply_category_tab(next);
                        }
                        _ => {}
                    },
                    InputMode::AddUrl => match key.code {
                        KeyCode::Enter => {
                            if !app.input.trim().is_empty() {
                                let parts: Vec<&str> = app.input.splitn(2, char::is_whitespace).collect();
                                let url = parts[0].to_string();
                                let path = parts
                                    .get(1)
                                    .map(|p| PathBuf::from(*p))
                                    .unwrap_or_else(|| {
                                        let filename = url.split('/').next_back().unwrap_or("download.bin");
                                        let filename = filename.split('?').next().unwrap_or("download.bin");
                                        PathBuf::from(filename)
                                    });
                                let _ = app.bridge.add(url, path);
                                app.status_message = "Added download.".into();
                                app.needs_refresh = true;
                            }
                            app.input.clear();
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Esc => {
                            app.input.clear();
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Char(c) => app.input.push(c),
                        KeyCode::Backspace => {
                            app.input.pop();
                        }
                        _ => {}
                    },
                    InputMode::Search => match key.code {
                        KeyCode::Enter => {
                            app.search = app.input.trim().to_string();
                            app.needs_refresh = true;
                            app.input.clear();
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Esc => {
                            app.input.clear();
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Char(c) => app.input.push(c),
                        KeyCode::Backspace => {
                            app.input.pop();
                        }
                        _ => {}
                    },
                }
            }
        }
    }
}
