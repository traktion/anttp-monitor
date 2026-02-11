use std::io;
use std::time::{Duration, Instant};
use anyhow::Result;
use chrono::Utc;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap},
    Frame, Terminal,
};
use tonic::transport::Channel;

pub mod command {
    tonic::include_proto!("command");
}

use command::command_service_client::CommandServiceClient;
use command::{Command, GetCommandsRequest};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FilterMode {
    Default,   // Waiting or Running
    Waiting,
    Running,
    Completed,
    Aborted,
    All,
}

struct App {
    commands: Vec<Command>,
    table_state: TableState,
    filter_mode: FilterMode,
    selected_command: Option<Command>,
    client: Option<CommandServiceClient<Channel>>,
    last_tick: Instant,
}

impl App {
    fn new(client: Option<CommandServiceClient<Channel>>) -> App {
        App {
            commands: Vec::new(),
            table_state: TableState::default(),
            filter_mode: FilterMode::Default,
            selected_command: None,
            client,
            last_tick: Instant::now(),
        }
    }

    fn filtered_commands(&self) -> Vec<&Command> {
        self.commands
            .iter()
            .filter(|c| {
                let state = c.state.to_ascii_lowercase();
                match self.filter_mode {
                    FilterMode::Default => state == "waiting" || state == "running",
                    FilterMode::Waiting => state == "waiting",
                    FilterMode::Running => state == "running",
                    FilterMode::Completed => state == "completed",
                    FilterMode::Aborted => state == "aborted",
                    FilterMode::All => true,
                }
            })
            .collect()
    }

    fn next(&mut self) {
        let count = self.filtered_commands().len();
        if count == 0 {
            self.table_state.select(None);
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= count - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    fn previous(&mut self) {
        let count = self.filtered_commands().len();
        if count == 0 {
            self.table_state.select(None);
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    count.saturating_sub(1)
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    async fn refresh_commands(&mut self) -> Result<()> {
        if let Some(client) = &mut self.client {
            let request = tonic::Request::new(GetCommandsRequest {});
            let response = client.get_commands(request).await?;
            self.commands = response.into_inner().commands;
        }
        Ok(())
    }
}

fn format_id(id: &str) -> String {
    if id.len() <= 6 {
        id.to_string()
    } else {
        format!("{}..{}", &id[..3], &id[id.len() - 3..])
    }
}

fn format_duration_ms(ms: u64) -> String {
    let secs = ms as f64 / 1000.0;
    format!("{secs:.3}")
}

fn compute_durations(waiting_at: Option<u64>, running_at: Option<u64>, terminated_at: Option<u64>, now_ms: u64) -> (String, String, String) {
    // Waiting duration
    let waiting_str = match waiting_at.filter(|w| *w > 0) {
        Some(w) => {
            let end = running_at.filter(|r| *r > 0).unwrap_or(now_ms);
            let dur = end.saturating_sub(w);
            format_duration_ms(dur)
        }
        None => "-".to_string(),
    };

    // Running duration
    let running_str = match running_at.filter(|r| *r > 0) {
        Some(r) => {
            let end = terminated_at.filter(|t| *t > 0).unwrap_or(now_ms);
            let dur = end.saturating_sub(r);
            format_duration_ms(dur)
        }
        None => "-".to_string(),
    };

    // Completed/Aborted ago
    let completed_str = match terminated_at.filter(|t| *t > 0) {
        Some(t) => {
            let dur = now_ms.saturating_sub(t);
            format_duration_ms(dur)
        }
        None => "-".to_string(),
    };

    (waiting_str, running_str, completed_str)
}

#[tokio::main]
async fn main() -> Result<()> {
    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // create app and run it
    let client = CommandServiceClient::connect("http://localhost:18887").await.ok();
    let mut app = App::new(client);

    let res = run_app(&mut terminal, &mut app).await;

    // restore terminal
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

async fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> io::Result<()> {
    let tick_rate = Duration::from_millis(1000);
    loop {
        if app.last_tick.elapsed() >= tick_rate {
            let _ = app.refresh_commands().await;
            app.last_tick = Instant::now();
        }

        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if app.selected_command.is_some() {
                    match key.code {
                        KeyCode::Enter | KeyCode::Left | KeyCode::Backspace => {
                            app.selected_command = None;
                        }
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Down | KeyCode::Char('j') => app.next(),
                    KeyCode::Up | KeyCode::Char('k') => app.previous(),
                    KeyCode::Enter => {
                        let filtered = app.filtered_commands();
                        if let Some(index) = app.table_state.selected() {
                            if let Some(cmd) = filtered.get(index) {
                                app.selected_command = Some((*cmd).clone());
                            }
                        }
                    }
                    KeyCode::Char('w') => {
                        app.filter_mode = FilterMode::Waiting;
                        app.table_state.select(Some(0));
                    }
                    KeyCode::Char('r') => {
                        app.filter_mode = FilterMode::Running;
                        app.table_state.select(Some(0));
                    }
                    KeyCode::Char('c') => {
                        app.filter_mode = FilterMode::Completed;
                        app.table_state.select(Some(0));
                    }
                    KeyCode::Char('b') => {
                        app.filter_mode = FilterMode::Aborted;
                        app.table_state.select(Some(0));
                    }
                    KeyCode::Char('a') => {
                        app.filter_mode = FilterMode::All;
                        app.table_state.select(Some(0));
                    }
                    KeyCode::Char('d') => {
                        app.filter_mode = FilterMode::Default;
                        app.table_state.select(Some(0));
                    }
                    _ => {}
                }
            }
        }
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    let rects = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0)].as_ref())
        .split(f.area());

    let selected_style = Style::default().add_modifier(Modifier::REVERSED);
    let normal_style = Style::default().bg(Color::Blue);
    let header_cells = ["ID", "Name", "State", "Waiting", "Running", "Completed/Aborted"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow)));
    let header = Row::new(header_cells)
        .style(normal_style)
        .height(1)
        .bottom_margin(1);

    let filtered = app.filtered_commands();
    let now_ms = (Utc::now().timestamp_millis()) as u64;
    let rows: Vec<Row> = filtered.iter().map(|item| {
        let (wait_str, run_str, comp_str) = compute_durations(
            if item.waiting_at > 0 { Some(item.waiting_at) } else { None },
            item.running_at,
            item.terminated_at,
            now_ms,
        );
        let cells = vec![
            Cell::from(format_id(&item.id)),
            Cell::from(item.name.clone()),
            Cell::from(item.state.clone()),
            Cell::from(wait_str),
            Cell::from(run_str),
            Cell::from(comp_str),
        ];
        Row::new(cells).height(1)
    }).collect();

    let t = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Min(20),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(18),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(format!(
        " AntTP Monitor - Mode: {} ",
        match app.filter_mode {
            FilterMode::Default => "Default (W/R)",
            FilterMode::Waiting => "Waiting",
            FilterMode::Running => "Running",
            FilterMode::Completed => "Completed",
            FilterMode::Aborted => "Aborted",
            FilterMode::All => "All",
        }
    )))
    .row_highlight_style(selected_style)
    .highlight_symbol(">> ");

    f.render_stateful_widget(t, rects[0], &mut app.table_state);

    if let Some(cmd) = &app.selected_command {
        let block = Block::default()
            .title(" Command Details ")
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::Black));
        let area = centered_rect(60, 60, f.area());
        f.render_widget(Clear, area); //this clears out the background
        f.render_widget(block, area);

        let details_layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints(
                [
                    Constraint::Length(1), // ID
                    Constraint::Length(1), // Name
                    Constraint::Length(1), // State
                    Constraint::Length(1), // Waiting
                    Constraint::Length(1), // Running
                    Constraint::Length(1), // Completed/Aborted
                    Constraint::Length(1), // Empty
                    Constraint::Length(1), // Properties Header
                    Constraint::Min(0),    // Properties list
                ]
                .as_ref(),
            )
            .split(area);

        f.render_widget(Paragraph::new(format!("ID: {}", cmd.id)), details_layout[0]);
        f.render_widget(
            Paragraph::new(format!("Name: {}", cmd.name)),
            details_layout[1],
        );
        f.render_widget(
            Paragraph::new(format!("State: {}", cmd.state)),
            details_layout[2],
        );
        let now_ms = (Utc::now().timestamp_millis()) as u64;
        let (wait_str, run_str, comp_str) = compute_durations(
            if cmd.waiting_at > 0 { Some(cmd.waiting_at) } else { None },
            cmd.running_at,
            cmd.terminated_at,
            now_ms,
        );
        f.render_widget(
            Paragraph::new(format!("Waiting: {} s", wait_str)),
            details_layout[3],
        );
        f.render_widget(
            Paragraph::new(format!("Running: {} s", run_str)),
            details_layout[4],
        );
        f.render_widget(
            Paragraph::new(format!("Completed/Aborted: {} s", comp_str)),
            details_layout[5],
        );

        f.render_widget(
            Paragraph::new("Properties:").style(Style::default().add_modifier(Modifier::BOLD)),
            details_layout[7],
        );

        let props_text: Vec<String> = cmd
            .properties
            .iter()
            .map(|p| format!("{}: {}", p.name, p.value))
            .collect();
        let props_paragraph = Paragraph::new(props_text.join("\n")).wrap(Wrap { trim: true });
        f.render_widget(props_paragraph, details_layout[8]);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ]
            .as_ref(),
        )
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_id() {
        assert_eq!(format_id("123456789"), "123..789");
        assert_eq!(format_id("123"), "123");
        assert_eq!(format_id("123456"), "123456");
    }

    #[test]
    fn test_compute_durations_examples() {
        // Example 1
        let now = 1_770_846_698u64; // current time (ms epoch)
        let waiting_at = Some(1_770_836_575u64);
        let running_at = None;
        let terminated_at = None;
        let (w, r, c) = compute_durations(waiting_at, running_at, terminated_at, now);
        assert_eq!(w, "10.123");
        assert_eq!(r, "-");
        assert_eq!(c, "-");

        // Example 2
        let now = 1_770_840_000u64;
        let waiting_at = Some(1_770_810_000u64);
        let running_at = Some(1_770_820_000u64);
        let terminated_at = None;
        let (w, r, c) = compute_durations(waiting_at, running_at, terminated_at, now);
        assert_eq!(w, "10.000");
        assert_eq!(r, "20.000");
        assert_eq!(c, "-");

        // Example 3
        let now = 1_770_850_000u64;
        let waiting_at = Some(1_770_810_000u64);
        let running_at = Some(1_770_820_000u64);
        let terminated_at = Some(1_770_830_000u64);
        let (w, r, c) = compute_durations(waiting_at, running_at, terminated_at, now);
        assert_eq!(w, "10.000");
        assert_eq!(r, "10.000");
        assert_eq!(c, "20.000");
    }
}
