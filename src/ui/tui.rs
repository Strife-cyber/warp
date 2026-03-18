use crate::registry::Registry;
use crate::ui::backend::{UiBackend, UiMessage};
use crate::utils::HumanBytes;
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

struct App {
    input: String,
    input_mode: InputMode,
    backend: UiBackend,
    table_state: TableState,
}

impl App {
    fn new(backend: UiBackend) -> Self {
        Self {
            input: String::new(),
            input_mode: InputMode::Normal,
            backend,
            table_state: TableState::default(),
        }
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
                InputMode::Normal => "q: Quit | a: Add | p: Pause | r: Resume | d: Delete | Up/Down: Select",
                InputMode::Editing => "Editing Mode: Enter to submit, Esc to cancel",
            };

            let help_block = Block::default()
                .title(Span::styled(" Warp TUI ", title_style))
                .borders(Borders::ALL);
            let help_para = Paragraph::new(help_msg).block(help_block);
            f.render_widget(help_para, chunks[0]);

            let state = app.backend.state.read().unwrap();
            let mut items: Vec<(String, crate::ui::backend::DownloadProgress)> = state.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            items.sort_by(|a, b| a.1.target_path.cmp(&b.1.target_path));



            let mut rows = Vec::new();
            for (_, progress) in &items {
                let status_str = match progress.status {
                    crate::registry::DownloadStatus::Downloading => "Downloading",
                    crate::registry::DownloadStatus::Paused => "Paused",
                    crate::registry::DownloadStatus::Error(_) => "Error",
                    crate::registry::DownloadStatus::Completed => "Completed",
                    crate::registry::DownloadStatus::Pending => "Pending",
                };
                let status_color = match progress.status {
                    crate::registry::DownloadStatus::Downloading => Color::Green,
                    crate::registry::DownloadStatus::Paused => Color::Yellow,
                    crate::registry::DownloadStatus::Error(_) => Color::Red,
                    crate::registry::DownloadStatus::Completed => Color::LightBlue,
                    crate::registry::DownloadStatus::Pending => Color::DarkGray,
                };
                
                let speed_str = if progress.status == crate::registry::DownloadStatus::Downloading {
                    format!("{}/s", HumanBytes(progress.speed))
                } else {
                    "-".to_string()
                };

                let frac = if progress.total > 0 {
                    progress.downloaded as f32 / progress.total as f32
                } else if progress.status == crate::registry::DownloadStatus::Completed {
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
            
            // Drop read lock before await/handling events!
            drop(state);

            if let InputMode::Editing = app.input_mode {
                f.set_cursor_position((
                    chunks[2].x + app.input.len() as u16 + 1,
                    chunks[2].y + 1,
                ));
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
                        InputMode::Normal => match key.code {
                            KeyCode::Char('q') => {
                            let _ = app.backend.tx.try_send(UiMessage::Quit);
                            return Ok(());
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

