// Declare our two modules so main.rs can see them.
// These names must match the filenames: sentence.rs and app.rs.
mod app;
mod sentence;

use app::App;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState},
};
use std::io;

// Restore the terminal: leave raw mode and the alternate screen. Safe to call
// more than once (e.g. from both the panic hook and normal teardown).
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
}

// Make sure a panic doesn't leave the terminal in raw/alt-screen mode (which
// would render the shell unusable). We restore first, then run the default
// hook so the panic message is printed normally.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));
}

fn main() -> io::Result<()> {
    // Load config *before* taking over the terminal, so a parse error prints
    // cleanly instead of from inside the alternate screen.
    let mut app = App::new(sentence::load());

    // Load practice stats and record this session: count the launch and mark
    // today as practiced (which maintains the streak). Persist immediately so
    // the session/streak survives even an abrupt exit.
    app.stats = sentence::load_stats();
    app.stats.sessions += 1;
    app.stats.touch_today();
    let _ = sentence::save_stats(&app.stats);

    // --- SETUP: take over the terminal ---
    install_panic_hook();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // --- THE MAIN LOOP ---
    let result = run(&mut terminal, &mut app);

    // --- TEARDOWN: give the terminal back ---
    restore_terminal();
    terminal.show_cursor()?;

    result
}

// Fold whole seconds of accumulated play time into the persisted stats and save
// them. The sub-second remainder is kept in `play_accum` so nothing is lost
// across repeated flushes (e.g. stopping and restarting auto-play).
fn flush_play_time(app: &mut App, play_accum: &mut f64) {
    let whole = play_accum.trunc();
    app.stats.add_play_secs(whole as u64);
    *play_accum -= whole;
    let _ = sentence::save_stats(&app.stats);
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> io::Result<()> {
    use std::time::{Duration, Instant};

    // When the gap before the *next* sentence started ticking. Armed only once
    // the current sentence has finished speaking (None while it's still being
    // read), so a long sentence is never cut off by the pause timer.
    let mut pause_start: Option<Instant> = None;

    // Accumulated wall-clock time (seconds) spent in auto-play this session but
    // not yet folded into the persisted stats. `last_tick` is the reference for
    // measuring each loop's elapsed time.
    let mut play_accum: f64 = 0.0;
    let mut last_tick = Instant::now();

    loop {
        // Credit time toward practice whenever auto-play is on.
        let now = Instant::now();
        if app.playing {
            play_accum += now.duration_since(last_tick).as_secs_f64();
        }
        last_tick = now;

        let matches = app.matches();
        // Reap a finished `say` process and learn which row is speaking.
        let speaking = app.poll_speaking();
        // Track the pause window: reset while speaking, start it the moment the
        // sentence finishes.
        if app.playing {
            if speaking.is_some() {
                pause_start = None;
            } else if pause_start.is_none() {
                pause_start = Some(Instant::now());
            }
        }
        // How far through the current auto-play pause we are, in [0, 1]. Full
        // while a sentence is still being read (no countdown yet).
        let progress = match pause_start {
            Some(t) if app.playing => {
                (t.elapsed().as_secs_f64() / app.pause_secs.max(1) as f64).clamp(0.0, 1.0)
            }
            _ => 0.0,
        };

        // 1. DRAW — dispatch on the chosen layout.
        terminal.draw(|frame| match app.layout() {
            sentence::Layout::Classic => draw_classic(frame, app, &matches, speaking, progress),
            sentence::Layout::Stacked => draw_stacked(frame, app, &matches, speaking, progress),
        })?;

        // 2. POLL for a key for up to 100ms (instead of blocking forever).
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            match app.mode {
                app::Mode::Normal => match key.code {
                    KeyCode::Char('q') => {
                        app.stop();
                        flush_play_time(app, &mut play_accum);
                        return Ok(());
                    }
                    KeyCode::Down | KeyCode::Right | KeyCode::Char('j') => app.next(),
                    KeyCode::Up | KeyCode::Left | KeyCode::Char('k') => app.previous(),
                    KeyCode::Enter | KeyCode::Char('p') => app.speak(),
                    KeyCode::Char('/') => app.mode = app::Mode::Search,
                    KeyCode::Char('a') => app.start_add(),
                    KeyCode::Char('e') => app.start_edit(),
                    KeyCode::Char('d') => app.start_delete(),
                    KeyCode::Char('m') => app.toggle_star(),
                    KeyCode::Char('s') | KeyCode::Char('S') => app.start_settings(),
                    KeyCode::Char('t') | KeyCode::Char('T') => app.mode = app::Mode::Stats,
                    KeyCode::Char('?') => app.mode = app::Mode::Help,
                    KeyCode::Char(' ') => {
                        app.playing = !app.playing;
                        if app.playing {
                            app.filter.clear(); // play uses ALL sentences
                            app.mode = app::Mode::Normal;
                            app.reshuffle();
                            app.advance(); // start the first sentence right away
                            pause_start = None; // pause begins only once it ends
                        } else {
                            app.stop(); // pausing: cut the current sentence
                            pause_start = None;
                            // Persist the time just spent playing.
                            flush_play_time(app, &mut play_accum);
                        }
                    }
                    KeyCode::Char('+') | KeyCode::Char('=') => {
                        app.pause_secs += 1;
                    }
                    KeyCode::Char('-') => {
                        if app.pause_secs > 1 {
                            app.pause_secs -= 1;
                        }
                    }
                    KeyCode::Char('>') | KeyCode::Char('.') => app.adjust_rate(1),
                    KeyCode::Char('<') | KeyCode::Char(',') => app.adjust_rate(-1),
                    _ => {}
                },
                app::Mode::Search => match key.code {
                    KeyCode::Esc => {
                        app.filter.clear();
                        app.selected = 0;
                        app.mode = app::Mode::Normal;
                    }
                    KeyCode::Enter => app.mode = app::Mode::Normal,
                    KeyCode::Backspace => {
                        app.filter.pop();
                        app.selected = 0;
                    }
                    KeyCode::Char(c) => {
                        app.filter.push(c);
                        app.selected = 0;
                    }
                    _ => {}
                },
                app::Mode::Add => match key.code {
                    KeyCode::Esc => app.cancel_add(),
                    KeyCode::Enter => app.add_enter(),
                    KeyCode::Backspace => app.add_backspace(),
                    KeyCode::Char(c) => app.add_char(c),
                    _ => {}
                },
                app::Mode::ConfirmDelete => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm_delete(),
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => app.cancel_delete(),
                    _ => {}
                },
                // Any key dismisses the help overlay.
                app::Mode::Help => app.mode = app::Mode::Normal,
                // In the stats overlay, arrows flip the chart metric; any other
                // key closes it.
                app::Mode::Stats => match key.code {
                    KeyCode::Left | KeyCode::Right => {
                        app.stats_chart_sentences = !app.stats_chart_sentences;
                    }
                    _ => app.mode = app::Mode::Normal,
                },
                app::Mode::Settings => match key.code {
                    KeyCode::Esc => app.cancel_settings(),
                    KeyCode::Enter => app.settings_enter(),
                    KeyCode::Backspace => app.settings_backspace(),
                    // The layout field is a toggle: arrows (or space) flip it.
                    KeyCode::Left | KeyCode::Right => app.settings_toggle_layout(),
                    KeyCode::Char(' ') if app.set_field == 3 => app.settings_toggle_layout(),
                    KeyCode::Char(c) => app.settings_char(c),
                    _ => {}
                },
            }
        }

        // 3. TICK: once the current sentence has finished AND the pause has
        // fully elapsed, speak the next one.
        if app.playing
            && speaking.is_none()
            && pause_start.is_some_and(|t| t.elapsed() >= Duration::from_secs(app.pause_secs))
        {
            app.advance();
            pause_start = None; // re-armed when this sentence finishes
        }
    }
}

// --- Classic layout: list on the left, detail panel on the right. ---
fn draw_classic(
    frame: &mut ratatui::Frame,
    app: &App,
    matches: &[usize],
    speaking: Option<usize>,
    progress: f64,
) {
    use ratatui::layout::{Constraint, Direction, Layout};

    // Three rows: top bar, main area, footer. While playing the top bar is one
    // row taller to fit the sentence being read below the gauge.
    let top_height = if app.playing { 4 } else { 3 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top_height),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

    // Top: a pause gauge (+ the sentence being read) while playing, else the
    // search box.
    if app.playing {
        let top = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Length(1)])
            .split(chunks[0]);
        render_gauge(frame, top[0], app, progress);
        render_now_playing(frame, top[1], app);
    } else {
        render_search_box(frame, chunks[0], app, true);
    }

    // Middle: list (left) + detail panel (right).
    let middle = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(chunks[1]);
    render_list(frame, middle[0], app, matches, speaking);
    render_detail(frame, middle[1], app, false);

    render_footer(frame, chunks[2], app);
    render_overlays(frame, app);
}

// --- Stacked layout: one vertical pile (header, detail, filter, list). ---
fn draw_stacked(
    frame: &mut ratatui::Frame,
    app: &App,
    matches: &[usize],
    speaking: Option<usize>,
    progress: f64,
) {
    use ratatui::layout::{Constraint, Direction, Layout};

    // header, detail, filter, list, footer.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(6),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

    // Header: the pause gauge while playing, else a small status bar.
    if app.playing {
        render_gauge(frame, chunks[0], app, progress);
    } else {
        render_status_header(frame, chunks[0], app);
    }
    render_detail(frame, chunks[1], app, true);
    render_search_box(frame, chunks[2], app, false);
    render_list(frame, chunks[3], app, matches, speaking);
    render_footer(frame, chunks[4], app);
    render_overlays(frame, app);
}

// The PLAYING pause gauge (shared by both layouts).
fn render_gauge(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &App, progress: f64) {
    use ratatui::widgets::Gauge;
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(format!(
            "PLAYING  ·  pause {}s (+/-)  ·  speed {}wpm (</>)  ·  space stop",
            app.pause_secs,
            app.rate_wpm()
        )))
        .gauge_style(Style::default().fg(Color::Cyan))
        .ratio(progress)
        .label(format!("{:.0}%", progress * 100.0));
    frame.render_widget(gauge, area);
}

// The "🔊 <sentence>" line shown below the gauge in the classic layout.
fn render_now_playing(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &App) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;
    let line = app
        .now_playing()
        .map(|s| {
            Line::from(vec![
                Span::raw("🔊 "),
                Span::styled(
                    s.text.clone(),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
            ])
        })
        .unwrap_or_else(|| Line::from(Span::styled("🔊 …", Style::default().fg(Color::DarkGray))));
    frame.render_widget(Paragraph::new(line), area);
}

// The idle status bar at the top of the stacked layout: the pause/speed
// controls (which live in the search-box title in the classic layout).
fn render_status_header(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &App) {
    use ratatui::widgets::Paragraph;
    let line = format!(
        "pause {}s (+/-)  ·  speed {}wpm (</>)",
        app.pause_secs,
        app.rate_wpm()
    );
    frame.render_widget(
        Paragraph::new(line)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title("say2")),
        area,
    );
}

// The search / filter input box. When `with_controls` is set (classic layout,
// where this box is the header) the pause/speed controls ride in the title;
// otherwise (stacked layout) the title stays short since a separate status
// header carries the controls.
fn render_search_box(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    app: &App,
    with_controls: bool,
) {
    use ratatui::widgets::Paragraph;
    let controls = if with_controls {
        format!(
            "  ·  pause {}s (+/-)  ·  speed {}wpm (</>)",
            app.pause_secs,
            app.rate_wpm()
        )
    } else {
        String::new()
    };
    let (title, line) = match app.mode {
        app::Mode::Search => (
            format!("Search (Esc exit){controls}"),
            format!("{}_", app.filter),
        ),
        _ => (format!("Filter{controls}"), app.filter.clone()),
    };
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

// The sentence list (shared by both layouts).
fn render_list(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    app: &App,
    matches: &[usize],
    speaking: Option<usize>,
) {
    use ratatui::text::{Line, Span};

    let items: Vec<ListItem> = matches
        .iter()
        .map(|&i| {
            let s = &app.sentences[i];
            let mut spans = Vec::new();
            if speaking == Some(i) {
                spans.push(Span::raw("🔊 "));
            }
            if s.starred {
                spans.push(Span::styled("★ ", Style::default().fg(Color::Yellow)));
            }
            spans.push(Span::raw(s.text.clone()));
            if !s.note.is_empty() {
                spans.push(Span::styled(
                    format!("  — {}", s.note),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(format!(
            "say2 ({}/{})",
            matches.len(),
            app.sentences.len()
        )))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut state = ListState::default();
    if !matches.is_empty() {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

// The footer shortcut line (shared by both layouts).
fn render_footer(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &App) {
    use ratatui::widgets::Paragraph;
    frame.render_widget(Paragraph::new(footer_line(app, area.width)), area);
}

// Whatever popup the current mode calls for, drawn on top of everything.
fn render_overlays(frame: &mut ratatui::Frame, app: &App) {
    match app.mode {
        app::Mode::Add => render_add_popup(frame, app),
        app::Mode::ConfirmDelete => render_confirm_popup(frame, app),
        app::Mode::Help => render_help_popup(frame),
        app::Mode::Settings => render_settings_popup(frame, app),
        app::Mode::Stats => render_stats_popup(frame, app),
        _ => {}
    }
}

// A Rect of `percent_x`% width and `lines` rows, centered in `area`.
fn centered_rect(percent_x: u16, lines: u16, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let width = area.width * percent_x / 100;
    let height = lines.min(area.height);
    ratatui::layout::Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

// Draw the centered "add a sentence" popup over the rest of the UI.
fn render_add_popup(frame: &mut ratatui::Frame, app: &App) {
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::widgets::{Clear, Paragraph};

    let area = centered_rect(60, 11, frame.area());

    // Clear whatever is underneath, then draw the bordered box.
    frame.render_widget(Clear, area);
    let title = if app.editing.is_some() {
        " Edit sentence "
    } else {
        " Add a sentence "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Stack: a label+input pair per field (phrase, tags, note), spacer, hint.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);

    let active = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);

    // Render one label+input pair. The focused field gets a bright label and a
    // trailing `_` cursor; the others stay dim.
    let mut field = |label: &str, value: &str, idx: usize, label_row: usize, input_row: usize| {
        let focused = app.add_field == idx;
        let label_style = if focused { active } else { dim };
        let shown = if focused {
            format!("{value}_")
        } else {
            value.to_string()
        };
        frame.render_widget(
            Paragraph::new(label.to_string()).style(label_style),
            rows[label_row],
        );
        frame.render_widget(Paragraph::new(shown), rows[input_row]);
    };

    field("Phrase", &app.add_text, 0, 0, 1);
    field(
        "Tags  (space-separated, # optional)",
        &app.add_tags,
        1,
        2,
        3,
    );
    field("Note  (optional comment)", &app.add_note, 2, 4, 5);

    let hint = if app.add_field == 2 {
        "Enter save  ·  Esc cancel"
    } else {
        "Enter next  ·  Esc cancel"
    };
    frame.render_widget(Paragraph::new(hint).style(dim), rows[7]);
}

// Draw the centered delete-confirmation popup over the rest of the UI.
fn render_confirm_popup(frame: &mut ratatui::Frame, app: &App) {
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::widgets::{Clear, Paragraph, Wrap};

    let area = centered_rect(60, 8, frame.area());

    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(" Delete sentence ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);

    // Show the sentence we're about to delete, wrapped if it's long.
    let text = app.selected_text().unwrap_or_default();
    frame.render_widget(
        Paragraph::new("Delete this sentence?")
            .style(Style::default().add_modifier(Modifier::BOLD)),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(format!("“{text}”"))
            .style(Style::default().fg(Color::Yellow))
            .wrap(Wrap { trim: true }),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new("y delete  ·  n / Esc cancel").style(Style::default().fg(Color::DarkGray)),
        rows[2],
    );
}

// A stable color for a tag, derived from its hash so the same tag always gets
// the same chip color across runs.
fn tag_color(tag: &str) -> Color {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    tag.hash(&mut h);
    const PALETTE: [Color; 6] = [
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
    ];
    PALETTE[(h.finish() % PALETTE.len() as u64) as usize]
}

// Accent color for footer keys: orange (256-color index 208), the warm
// complement to the app's cool cyan. Change this one constant to retheme.
const ACCENT: Color = Color::Indexed(208);

// The footer shortcut line for the current mode, with keys highlighted in the
// accent color and labels dimmed. Fits `max_width`: the `lead` essentials
// (help and quit) are anchored on the LEFT so they're always visible, and the
// rest is truncated with "…" on the right when there isn't room.
fn footer_line(app: &App, max_width: u16) -> ratatui::text::Line<'static> {
    use ratatui::text::{Line, Span};

    // A list of (key, label) shortcut pairs.
    type Hints = &'static [(&'static str, &'static str)];
    // `lead` is anchored left and never dropped; `body` is truncated on the right.
    let (lead, body): (Hints, Hints) = if app.playing {
        (
            &[("?", "help"), ("q", "quit")],
            &[("space", "stop"), ("+/-", "pause"), ("</>", "speed")],
        )
    } else {
        match app.mode {
            app::Mode::Search => (
                &[],
                &[("type", "filter"), ("Enter", "apply"), ("Esc", "clear")],
            ),
            app::Mode::Add | app::Mode::Settings => {
                (&[], &[("Enter", "next/save"), ("Esc", "cancel")])
            }
            app::Mode::ConfirmDelete => (&[], &[("y", "delete"), ("n / Esc", "cancel")]),
            app::Mode::Help => (&[], &[("any key", "close")]),
            app::Mode::Stats => (&[], &[("←/→", "metric"), ("any key", "close")]),
            app::Mode::Normal => (
                &[("?", "help"), ("q", "quit")],
                &[
                    ("j/k", "move"),
                    ("p", "speak"),
                    ("space", "play"),
                    ("/", "search"),
                    ("a", "add"),
                    ("e", "edit"),
                    ("d", "delete"),
                    ("m", "star"),
                    ("s", "settings"),
                    ("t", "stats"),
                    ("+/-", "pause"),
                    ("</>", "speed"),
                ],
            ),
        }
    };

    let key_style = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(Color::Gray);
    let sep_style = Style::default().fg(Color::DarkGray);

    // Spans for one "key label" item, optionally preceded by a separator.
    let item = |k: &str, v: &str, lead_sep: bool| {
        let mut out = Vec::new();
        if lead_sep {
            out.push(Span::styled("  ·  ", sep_style));
        }
        out.push(Span::styled(format!("{k} "), key_style));
        out.push(Span::styled(v.to_string(), label_style));
        out
    };
    let width = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();

    let max = max_width as usize;

    // The lead is always rendered first (anchored left).
    let mut spans: Vec<Span> = lead
        .iter()
        .enumerate()
        .flat_map(|(i, (k, v))| item(k, v, i > 0))
        .collect();
    let mut used = width(&spans);
    let has_lead = !lead.is_empty();

    // Fast path: everything fits.
    let body_spans: Vec<Span> = body
        .iter()
        .enumerate()
        .flat_map(|(i, (k, v))| item(k, v, has_lead || i > 0))
        .collect();
    if used + width(&body_spans) <= max {
        spans.extend(body_spans);
        return Line::from(spans);
    }

    // Otherwise add body items until the next one wouldn't leave room for "…".
    let ellipsis_w = width(&item("…", "", true));
    for (i, (k, v)) in body.iter().enumerate() {
        let piece = item(k, v, has_lead || i > 0);
        let w = width(&piece);
        if used + w + ellipsis_w > max {
            break;
        }
        used += w;
        spans.extend(piece);
    }
    spans.push(Span::styled("  ·  …", sep_style));
    Line::from(spans)
}

// Detail panel: the selected sentence, its tags as colored chips, and its
// note. `compact` drops the blank separator lines and inlines the star marker
// so it fits the short, full-width panel of the stacked layout; the roomier
// spaced-out version is used for the tall side panel of the classic layout.
fn render_detail(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &App, compact: bool) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Paragraph, Wrap};

    let block = Block::default().borders(Borders::ALL).title("Detail");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(s) = app.selected_sentence() else {
        frame.render_widget(
            Paragraph::new("No sentence selected.").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    };

    // The colored tag chips (or a dim "no tags"), reused by both modes.
    let tag_line = if s.tags.is_empty() {
        Line::from(Span::styled("no tags", Style::default().fg(Color::DarkGray)))
    } else {
        let mut spans = Vec::new();
        for tag in &s.tags {
            spans.push(Span::styled(
                format!(" {tag} "),
                Style::default().fg(Color::Black).bg(tag_color(tag)),
            ));
            spans.push(Span::raw(" "));
        }
        Line::from(spans)
    };

    let mut lines: Vec<Line> = Vec::new();
    if compact {
        // Sentence text with an inline star marker.
        let mut first = vec![Span::styled(
            s.text.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )];
        if s.starred {
            first.push(Span::styled("  ★", Style::default().fg(Color::Yellow)));
        }
        lines.push(Line::from(first));
        lines.push(tag_line);
        if !s.note.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("📝 {}", s.note),
                Style::default().fg(Color::Yellow),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            s.text.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        if s.starred {
            lines.push(Line::from(Span::styled(
                "★ starred (plays more often)",
                Style::default().fg(Color::Yellow),
            )));
        }
        lines.push(Line::raw(""));
        lines.push(tag_line);
        if !s.note.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                format!("📝 {}", s.note),
                Style::default().fg(Color::Yellow),
            )));
        }
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

// Centered help overlay listing every key binding.
fn render_help_popup(frame: &mut ratatui::Frame) {
    use ratatui::layout::Constraint;
    use ratatui::widgets::{Cell, Clear, Row, Table};

    let area = centered_rect(54, 16, frame.area());
    frame.render_widget(Clear, area);

    let rows = [
        ("j/k ←→ ↑↓", "move selection"),
        ("p / Enter", "speak selected"),
        ("space", "play / stop auto mode"),
        ("/", "search (text or tag)"),
        ("a", "add a sentence"),
        ("e", "edit selected"),
        ("d", "delete selected"),
        ("m", "star / unstar (plays more often)"),
        ("s", "settings (voice / rate / weight / layout)"),
        ("t", "stats (time practiced, streak, counts)"),
        ("+ / -", "pause length between sentences"),
        ("< / >", "speaking speed (words/min)"),
        ("?", "this help"),
        ("q", "quit"),
    ]
    .into_iter()
    .map(|(k, v)| {
        Row::new(vec![
            Cell::from(k).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Cell::from(v),
        ])
    });

    let table = Table::new(rows, [Constraint::Length(16), Constraint::Min(0)]).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Keys (any key to close) "),
    );
    frame.render_widget(table, area);
}

// Centered popup to edit voice / rate / star weight / layout. Empty text
// field = unset; layout is a toggle (←/→).
fn render_settings_popup(frame: &mut ratatui::Frame, app: &App) {
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::widgets::{Clear, Paragraph};

    let area = centered_rect(60, 13, frame.area());

    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Settings ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // A label+input pair per field (voice, rate, star weight, layout), then a
    // spacer and the hint.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);

    let active = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);

    let mut field = |label: &str, value: &str, idx: usize, label_row: usize, input_row: usize| {
        let focused = app.set_field == idx;
        let label_style = if focused { active } else { dim };
        let shown = if focused {
            format!("{value}_")
        } else {
            value.to_string()
        };
        frame.render_widget(
            Paragraph::new(label.to_string()).style(label_style),
            rows[label_row],
        );
        frame.render_widget(Paragraph::new(shown), rows[input_row]);
    };

    field(
        "Voice  (macOS `say -v`, empty = default)",
        &app.set_voice,
        0,
        0,
        1,
    );
    field("Rate  (words/min, empty = default)", &app.set_rate, 1, 2, 3);
    field("Star weight  (default 3)", &app.set_star_weight, 2, 4, 5);

    // Layout is a toggle rather than a text field: show the current choice,
    // flanked by arrows when focused.
    let layout_focused = app.set_field == 3;
    let layout_label_style = if layout_focused { active } else { dim };
    frame.render_widget(
        Paragraph::new("Layout  (←/→ to switch)").style(layout_label_style),
        rows[6],
    );
    let layout_value = match app.set_layout {
        sentence::Layout::Classic => "classic  (list + side panel)",
        sentence::Layout::Stacked => "stacked  (single vertical pile)",
    };
    let layout_shown = if layout_focused {
        format!("‹ {layout_value} ›")
    } else {
        layout_value.to_string()
    };
    frame.render_widget(Paragraph::new(layout_shown), rows[7]);

    let hint = if app.set_field == 3 {
        "Enter save  ·  Esc cancel"
    } else {
        "Enter next  ·  Esc cancel"
    };
    frame.render_widget(Paragraph::new(hint).style(dim), rows[9]);
}

// Day-of-month for a day number (days since 1970-01-01, UTC), via Howard
// Hinnant's civil-from-days algorithm. Used to label chart bars; within a
// 14-day window the day-of-month is unique, so it reads unambiguously.
fn day_of_month(day: u64) -> u32 {
    let z = day as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    (doy - (153 * mp + 2) / 5 + 1) as u32 // [1, 31]
}

// Format a duration in whole seconds as a compact "2h 15m" / "45m" / "30s".
fn format_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

// Centered overlay showing practice statistics: a summary table on top, and a
// bar chart of the last two weeks of usage below (minutes or sentences per day,
// toggled with ←/→).
fn render_stats_popup(frame: &mut ratatui::Frame, app: &App) {
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::widgets::{BarChart, Cell, Clear, Paragraph, Row, Table};

    let area = centered_rect(80, 22, frame.area());
    frame.render_widget(Clear, area);

    // Outer frame; everything else is drawn inside it.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Stats (any key to close) ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Summary table (5 rows), a chart caption, then the chart fills the rest.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);

    let st = &app.stats;
    let streak = if st.best_streak > st.current_streak {
        format!("{} days  (best {})", st.current_streak, st.best_streak)
    } else {
        format!("{} days", st.current_streak)
    };
    let table_rows = [
        ("⏱ Time practiced", format_duration(st.play_secs)),
        ("🗣 Sentences spoken", st.sentences_spoken.to_string()),
        ("📅 Days practiced", st.days_practiced.to_string()),
        ("🔥 Current streak", streak),
        ("▶ Sessions", st.sessions.to_string()),
    ]
    .into_iter()
    .map(|(k, v)| {
        Row::new(vec![
            Cell::from(k).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Cell::from(v),
        ])
    });
    let table = Table::new(table_rows, [Constraint::Length(22), Constraint::Min(0)]);
    frame.render_widget(table, rows[0]);

    // The last 14 days, oldest → today, with gaps filled as zero.
    let days = st.recent_days(14);
    let show_sentences = app.stats_chart_sentences;
    let caption = format!(
        "Last 14 days — {} per day   (←/→ to switch)",
        if show_sentences {
            "sentences"
        } else {
            "minutes"
        }
    );
    frame.render_widget(
        Paragraph::new(caption).style(Style::default().fg(Color::DarkGray)),
        rows[1],
    );

    let labels: Vec<String> = days.iter().map(|d| day_of_month(d.day).to_string()).collect();
    let data: Vec<(&str, u64)> = labels
        .iter()
        .zip(&days)
        .map(|(label, d)| {
            let value = if show_sentences {
                d.sentences
            } else {
                d.play_secs / 60
            };
            (label.as_str(), value)
        })
        .collect();

    let chart = BarChart::default()
        .data(&data[..])
        .bar_width(3)
        .bar_gap(1)
        .bar_style(Style::default().fg(Color::Cyan))
        .value_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .label_style(Style::default().fg(Color::DarkGray));
    frame.render_widget(chart, rows[2]);
}

#[cfg(test)]
mod tests {
    use super::{day_of_month, format_duration};

    #[test]
    fn format_duration_picks_the_right_unit() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(60), "1m 00s");
        assert_eq!(format_duration(125), "2m 05s");
        assert_eq!(format_duration(3600), "1h 00m");
        assert_eq!(format_duration(8100), "2h 15m");
    }

    #[test]
    fn day_of_month_matches_known_dates() {
        assert_eq!(day_of_month(0), 1); // 1970-01-01
        assert_eq!(day_of_month(30), 31); // 1970-01-31
        assert_eq!(day_of_month(31), 1); // 1970-02-01
        assert_eq!(day_of_month(10_957), 1); // 2000-01-01
    }
}
