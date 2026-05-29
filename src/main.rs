use std::{env, fs, io, io::BufRead, process, sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex}, thread, time::Duration};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};

#[derive(PartialEq, Clone, Copy)]
enum InputMode {
    Normal,
    Search,
    Filter,
}

struct App {
    lines: Arc<Mutex<Vec<String>>>,
    offset: usize,
    cursor: usize,
    filename: String,
    show_detail: bool,
    follow: bool,
    // Search
    input_mode: InputMode,
    input_buf: String,
    search_query: String,
    search_matches: Vec<usize>,
    search_match_idx: Option<usize>,
    // Filter
    filter_query: String,
    filtered_indices: Option<Vec<usize>>,
}

impl App {
    fn from_file(filename: &str) -> io::Result<Self> {
        let content = fs::read_to_string(filename)?;
        let lines: Vec<String> = content.lines().map(String::from).collect();
        Ok(Self {
            lines: Arc::new(Mutex::new(lines)),
            offset: 0,
            cursor: 0,
            filename: filename.to_string(),
            show_detail: false,
            follow: false,
            input_mode: InputMode::Normal,
            input_buf: String::new(),
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: None,
            filter_query: String::new(),
            filtered_indices: None,
        })
    }

    fn from_stdin(quit: Arc<AtomicBool>) -> Self {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let lines_clone = Arc::clone(&lines);

        thread::spawn(move || {
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                if quit.load(Ordering::Relaxed) {
                    break;
                }
                match line {
                    Ok(l) => lines_clone.lock().unwrap().push(l),
                    Err(_) => break,
                }
            }
        });

        Self {
            lines,
            offset: 0,
            cursor: 0,
            filename: "<stdin>".to_string(),
            show_detail: false,
            follow: true,
            input_mode: InputMode::Normal,
            input_buf: String::new(),
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: None,
            filter_query: String::new(),
            filtered_indices: None,
        }
    }

    fn line_count(&self) -> usize {
        self.lines.lock().unwrap().len()
    }

    fn get_line(&self, idx: usize) -> Option<String> {
        self.lines.lock().unwrap().get(idx).cloned()
    }

    /// Number of visible lines (filtered or total)
    fn visible_count(&self) -> usize {
        match &self.filtered_indices {
            Some(indices) => indices.len(),
            None => self.line_count(),
        }
    }

    /// Map a visible row index to the real line index
    fn visible_to_real(&self, vis: usize) -> usize {
        match &self.filtered_indices {
            Some(indices) => indices.get(vis).copied().unwrap_or(0),
            None => vis,
        }
    }

    /// Get visible lines for rendering
    fn get_visible_range(&self, start: usize, end: usize) -> Vec<(usize, String)> {
        let lines = self.lines.lock().unwrap();
        let count = match &self.filtered_indices {
            Some(indices) => indices.len(),
            None => lines.len(),
        };
        let end = end.min(count);
        let mut result = Vec::new();
        for vis in start..end {
            let real = match &self.filtered_indices {
                Some(indices) => indices[vis],
                None => vis,
            };
            if let Some(line) = lines.get(real) {
                result.push((real, line.clone()));
            }
        }
        result
    }

    fn max_line(&self) -> usize {
        self.visible_count().saturating_sub(1)
    }

    fn ensure_visible(&mut self, viewport_height: usize) {
        if self.cursor < self.offset {
            self.offset = self.cursor;
        } else if viewport_height > 0 && self.cursor >= self.offset + viewport_height {
            self.offset = self.cursor.saturating_sub(viewport_height - 1);
        }
    }

    fn cursor_down(&mut self, n: usize, viewport_height: usize) {
        self.cursor = (self.cursor + n).min(self.max_line());
        self.ensure_visible(viewport_height);
    }

    fn cursor_up(&mut self, n: usize, viewport_height: usize) {
        self.cursor = self.cursor.saturating_sub(n);
        self.ensure_visible(viewport_height);
    }

    fn goto_top(&mut self) {
        self.cursor = 0;
        self.offset = 0;
    }

    fn goto_bottom(&mut self, viewport_height: usize) {
        self.cursor = self.max_line();
        self.ensure_visible(viewport_height);
    }

    fn toggle_detail(&mut self) {
        self.show_detail = !self.show_detail;
    }

    fn selected_line_raw(&self) -> String {
        let real = self.visible_to_real(self.cursor);
        self.get_line(real).unwrap_or_default()
    }

    fn selected_line_formatted(&self) -> (String, bool) {
        let raw = self.selected_line_raw();
        let trimmed = raw.trim();
        if looks_like_json(trimmed) {
            (pretty_print_json(trimmed), true)
        } else {
            (raw.clone(), false)
        }
    }

    // --- Search ---

    fn do_search(&mut self) {
        let query_lower = self.search_query.to_lowercase();
        if query_lower.is_empty() {
            self.search_matches.clear();
            self.search_match_idx = None;
            return;
        }
        let lines = self.lines.lock().unwrap();
        self.search_matches.clear();
        // Search across visible lines
        let count = match &self.filtered_indices {
            Some(indices) => indices.len(),
            None => lines.len(),
        };
        for vis in 0..count {
            let real = match &self.filtered_indices {
                Some(indices) => indices[vis],
                None => vis,
            };
            if let Some(line) = lines.get(real) {
                if line.to_lowercase().contains(&query_lower) {
                    self.search_matches.push(vis);
                }
            }
        }
        // Jump to first match at or after cursor
        if !self.search_matches.is_empty() {
            self.search_match_idx = Some(
                self.search_matches
                    .iter()
                    .position(|&m| m >= self.cursor)
                    .unwrap_or(0),
            );
        } else {
            self.search_match_idx = None;
        }
    }

    fn search_next(&mut self, viewport_height: usize) {
        if self.search_matches.is_empty() {
            return;
        }
        let idx = match self.search_match_idx {
            Some(i) => (i + 1) % self.search_matches.len(),
            None => 0,
        };
        self.search_match_idx = Some(idx);
        self.cursor = self.search_matches[idx];
        self.ensure_visible(viewport_height);
    }

    fn search_prev(&mut self, viewport_height: usize) {
        if self.search_matches.is_empty() {
            return;
        }
        let idx = match self.search_match_idx {
            Some(0) | None => self.search_matches.len() - 1,
            Some(i) => i - 1,
        };
        self.search_match_idx = Some(idx);
        self.cursor = self.search_matches[idx];
        self.ensure_visible(viewport_height);
    }

    // --- Filter ---

    fn apply_filter(&mut self) {
        let query_lower = self.filter_query.to_lowercase();
        if query_lower.is_empty() {
            self.filtered_indices = None;
        } else {
            let lines = self.lines.lock().unwrap();
            let indices: Vec<usize> = (0..lines.len())
                .filter(|&i| lines[i].to_lowercase().contains(&query_lower))
                .collect();
            self.filtered_indices = Some(indices);
        }
        // Reset cursor
        self.cursor = 0;
        self.offset = 0;
        // Re-run search against new visible set
        if !self.search_query.is_empty() {
            self.do_search();
        }
    }

    fn clear_filter(&mut self) {
        self.filter_query.clear();
        self.filtered_indices = None;
        self.cursor = 0;
        self.offset = 0;
        if !self.search_query.is_empty() {
            self.do_search();
        }
    }
}

fn looks_like_json(s: &str) -> bool {
    let t = s.trim();
    (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']'))
}

fn pretty_print_json(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 2);
    let mut indent = 0usize;
    let mut in_string = false;
    let mut escape_next = false;
    let chars: Vec<char> = input.chars().collect();

    for i in 0..chars.len() {
        let c = chars[i];

        if escape_next {
            out.push(c);
            escape_next = false;
            continue;
        }

        if c == '\\' && in_string {
            out.push(c);
            escape_next = true;
            continue;
        }

        if c == '"' {
            in_string = !in_string;
            out.push(c);
            continue;
        }

        if in_string {
            out.push(c);
            continue;
        }

        match c {
            '{' | '[' => {
                out.push(c);
                let rest = &chars[i + 1..];
                let next_non_ws = rest.iter().find(|ch| !ch.is_ascii_whitespace());
                let close = if c == '{' { '}' } else { ']' };
                if next_non_ws == Some(&close) {
                    // empty container
                } else {
                    indent += 1;
                    out.push('\n');
                    push_indent(&mut out, indent);
                }
            }
            '}' | ']' => {
                let last_non_ws = out.chars().rev().find(|ch| !ch.is_ascii_whitespace());
                let open = if c == '}' { '{' } else { '[' };
                if last_non_ws == Some(open) {
                    while out.ends_with(|ch: char| ch.is_ascii_whitespace()) {
                        out.pop();
                    }
                    out.push(c);
                } else {
                    indent = indent.saturating_sub(1);
                    out.push('\n');
                    push_indent(&mut out, indent);
                    out.push(c);
                }
            }
            ',' => {
                out.push(',');
                out.push('\n');
                push_indent(&mut out, indent);
            }
            ':' => {
                out.push_str(": ");
            }
            ' ' | '\t' | '\r' | '\n' => {}
            _ => out.push(c),
        }
    }
    out
}

fn push_indent(s: &mut String, level: usize) {
    for _ in 0..level {
        s.push_str("  ");
    }
}

fn colorize_json_line(line: &str) -> Line<'_> {
    let mut spans: Vec<Span> = Vec::new();
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];

    if !indent.is_empty() {
        spans.push(Span::raw(indent));
    }

    if trimmed == "{" || trimmed == "}" {
        spans.push(Span::styled(trimmed, Style::default().fg(Color::White)));
        return Line::from(spans);
    }

    let mut rest = trimmed;

    if rest.starts_with('"') {
        if let Some(colon) = rest.find("\":") {
            let key_part = &rest[..colon + 2];
            spans.push(Span::styled(key_part, Style::default().fg(Color::Cyan)));
            rest = &rest[colon + 2..];
            if rest.starts_with(' ') {
                spans.push(Span::raw(" "));
                rest = &rest[1..];
            }
        }
    }

    let val = rest.trim_end_matches(',');
    let has_comma = rest.len() > val.len();

    if val.starts_with('"') {
        spans.push(Span::styled(val, Style::default().fg(Color::Green)));
    } else if val == "true" || val == "false" || val == "null" {
        spans.push(Span::styled(val, Style::default().fg(Color::Yellow)));
    } else if val.parse::<f64>().is_ok() {
        spans.push(Span::styled(val, Style::default().fg(Color::Magenta)));
    } else {
        spans.push(Span::styled(val, Style::default().fg(Color::White)));
    }

    if has_comma {
        spans.push(Span::styled(",", Style::default().fg(Color::DarkGray)));
    }

    Line::from(spans)
}

/// Render a line with search highlights
fn render_log_line<'a>(
    line: &'a str,
    lineno: usize,
    is_selected: bool,
    search_query: &str,
    is_search_match: bool,
) -> Line<'a> {
    let num_style = if is_selected {
        Style::default().fg(Color::Yellow).bold()
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let num_span = Span::styled(format!("{:>6} ", lineno), num_style);

    let mut spans = vec![num_span];

    let query_lower = search_query.to_lowercase();
    if !query_lower.is_empty() && is_search_match {
        // Highlight search matches in the line
        let line_lower = line.to_lowercase();
        let mut last = 0;
        let base_style = if is_selected {
            Style::default().bg(Color::DarkGray).fg(Color::White).bold()
        } else {
            Style::default()
        };
        let hl_style = Style::default().bg(Color::Yellow).fg(Color::Black);

        for (start, _) in line_lower.match_indices(&query_lower) {
            if start > last {
                spans.push(Span::styled(
                    line[last..start].to_string(),
                    base_style,
                ));
            }
            spans.push(Span::styled(
                line[start..start + query_lower.len()].to_string(),
                hl_style,
            ));
            last = start + query_lower.len();
        }
        if last < line.len() {
            spans.push(Span::styled(line[last..].to_string(), base_style));
        }
    } else {
        let content_style = if is_selected {
            Style::default().bg(Color::DarkGray).fg(Color::White).bold()
        } else {
            Style::default()
        };
        spans.push(Span::styled(line, content_style));
    }

    let line = Line::from(spans);
    if is_selected {
        line.style(Style::default().bg(Color::DarkGray))
    } else {
        line
    }
}

fn is_stdin_piped() -> bool {
    use std::os::unix::io::AsRawFd;
    unsafe { libc::isatty(io::stdin().as_raw_fd()) == 0 }
}

fn cleanup() {
    let _ = disable_raw_mode();
    let _ = io::stdout().execute(LeaveAlternateScreen);
}

fn open_in_editor(app: &App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) {
    let (content, is_json) = app.selected_line_formatted();
    let ext = if is_json { "json" } else { "txt" };

    let tmp_dir = env::temp_dir();
    let tmp_path = tmp_dir.join(format!("logoscope_line.{}", ext));
    if fs::write(&tmp_path, &content).is_err() {
        return;
    }

    let editor = env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());

    // Temporarily leave TUI so the editor can use the terminal
    cleanup();

    let _ = process::Command::new(&editor)
        .arg(&tmp_path)
        .status();

    // Restore TUI
    let _ = enable_raw_mode();
    let _ = io::stdout().execute(EnterAlternateScreen);
    let _ = terminal.clear();
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    let quit = Arc::new(AtomicBool::new(false));

    let mut app = if args.len() >= 2 {
        App::from_file(&args[1])?
    } else if is_stdin_piped() {
        App::from_stdin(Arc::clone(&quit))
    } else {
        eprintln!("Usage: logoscope <logfile>");
        eprintln!("       command | logoscope");
        process::exit(1);
    };

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        cleanup();
        default_hook(info);
    }));

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let result = run(&mut app, &mut terminal);

    quit.store(true, Ordering::Relaxed);
    cleanup();

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        process::exit(1);
    }

    process::exit(0);
}

fn run(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            // Layout: log panel + optional detail + optional input bar
            let has_input = app.input_mode != InputMode::Normal;
            let (main_area, input_area) = if has_input {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(3), Constraint::Length(1)])
                    .split(area);
                (chunks[0], Some(chunks[1]))
            } else {
                (area, None)
            };

            let (log_area, detail_area) = if app.show_detail {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(main_area);
                (chunks[0], Some(chunks[1]))
            } else {
                (main_area, None)
            };

            // --- Log panel ---
            let viewport_height = log_area.height.saturating_sub(2) as usize;
            let total = app.visible_count();
            let end = (app.offset + viewport_height).min(total);
            let visible_lines = app.get_visible_range(app.offset, end);

            let search_query = app.search_query.clone();
            let search_matches = &app.search_matches;

            let visible: Vec<Line> = visible_lines
                .iter()
                .enumerate()
                .map(|(i, (real_idx, line))| {
                    let vis_idx = app.offset + i;
                    let lineno = real_idx + 1;
                    let is_selected = vis_idx == app.cursor;
                    let is_match = search_matches.contains(&vis_idx);

                    render_log_line(line.as_str(), lineno, is_selected, &search_query, is_match)
                })
                .collect();

            let follow_indicator = if app.follow { " [FOLLOW]" } else { "" };
            let filter_indicator = match &app.filtered_indices {
                Some(indices) => format!(" [FILTER: {} matches]", indices.len()),
                None => String::new(),
            };
            let search_indicator = if !app.search_matches.is_empty() {
                let pos = app.search_match_idx.map(|i| i + 1).unwrap_or(0);
                format!(" [{}/{}]", pos, app.search_matches.len())
            } else if !app.search_query.is_empty() {
                " [no matches]".to_string()
            } else {
                String::new()
            };

            let title = format!(
                " {} — line {}/{}{}{}{} ",
                app.filename,
                if total > 0 { app.cursor + 1 } else { 0 },
                total,
                follow_indicator,
                filter_indicator,
                search_indicator,
            );

            let paragraph = Paragraph::new(visible).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(Style::default().fg(Color::Blue)),
            );
            frame.render_widget(paragraph, log_area);

            let max_offset = total.saturating_sub(viewport_height);
            let mut scrollbar_state =
                ScrollbarState::new(max_offset).position(app.offset);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                log_area,
                &mut scrollbar_state,
            );

            // --- Detail panel ---
            if let Some(detail_area) = detail_area {
                let (formatted, is_json) = app.selected_line_formatted();

                if is_json {
                    let detail_lines: Vec<Line> = formatted
                        .lines()
                        .map(|l| colorize_json_line(l))
                        .collect();

                    let detail = Paragraph::new(detail_lines).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" JSON ")
                            .border_style(Style::default().fg(Color::Magenta)),
                    );
                    frame.render_widget(detail, detail_area);
                } else {
                    let detail = Paragraph::new(formatted.as_str())
                        .wrap(ratatui::widgets::Wrap { trim: false })
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(" Text ")
                                .border_style(Style::default().fg(Color::Cyan)),
                        );
                    frame.render_widget(detail, detail_area);
                }
            }

            // --- Input bar ---
            if let Some(input_area) = input_area {
                let (prefix, style) = match app.input_mode {
                    InputMode::Search => ("/", Style::default().fg(Color::Yellow)),
                    InputMode::Filter => ("\\", Style::default().fg(Color::Green)),
                    InputMode::Normal => unreachable!(),
                };
                let input_line = Line::from(vec![
                    Span::styled(prefix, style.bold()),
                    Span::styled(app.input_buf.as_str(), style),
                ]);
                frame.render_widget(Paragraph::new(input_line), input_area);
                // Show cursor
                frame.set_cursor_position((
                    input_area.x + 1 + app.input_buf.len() as u16,
                    input_area.y,
                ));
            }
        })?;

        let total_height = terminal.size()?.height as usize;
        let input_height = if app.input_mode != InputMode::Normal { 1 } else { 0 };
        let log_height = if app.show_detail {
            ((total_height - input_height) / 2).saturating_sub(2)
        } else {
            (total_height - input_height).saturating_sub(2)
        };

        // Auto-follow
        if app.follow {
            let total = app.visible_count();
            if total > 0 {
                app.cursor = total - 1;
                app.ensure_visible(log_height);
            }
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }

        if let Event::Key(key) = event::read()? {
            match app.input_mode {
                InputMode::Normal => {
                    let half_page = log_height / 2;
                    app.follow = false;

                    match (key.modifiers, key.code) {
                        (_, KeyCode::Char('q')) | (KeyModifiers::CONTROL, KeyCode::Char('c')) => return Ok(()),

                        (_, KeyCode::Char('j')) | (_, KeyCode::Down) => app.cursor_down(1, log_height),
                        (_, KeyCode::Char('k')) | (_, KeyCode::Up) => app.cursor_up(1, log_height),

                        (KeyModifiers::CONTROL, KeyCode::Char('d')) => app.cursor_down(half_page, log_height),
                        (KeyModifiers::CONTROL, KeyCode::Char('u')) => app.cursor_up(half_page, log_height),

                        (KeyModifiers::CONTROL, KeyCode::Char('f')) | (_, KeyCode::PageDown) => {
                            app.cursor_down(log_height, log_height)
                        }
                        (KeyModifiers::CONTROL, KeyCode::Char('b')) | (_, KeyCode::PageUp) => {
                            app.cursor_up(log_height, log_height)
                        }

                        (_, KeyCode::Char('g')) => app.goto_top(),
                        (_, KeyCode::Char('G')) => {
                            app.goto_bottom(log_height);
                            app.follow = true;
                        }
                        (_, KeyCode::Char('F')) => app.follow = true,
                        (_, KeyCode::Home) => app.goto_top(),
                        (_, KeyCode::End) => {
                            app.goto_bottom(log_height);
                            app.follow = true;
                        }

                        // Search
                        (_, KeyCode::Char('/')) => {
                            app.input_mode = InputMode::Search;
                            app.input_buf.clear();
                        }
                        (_, KeyCode::Char('n')) => app.search_next(log_height),
                        (_, KeyCode::Char('N')) => app.search_prev(log_height),

                        // Filter
                        (_, KeyCode::Char('\\')) => {
                            app.input_mode = InputMode::Filter;
                            app.input_buf = app.filter_query.clone();
                        }

                        // Toggle detail panel
                        (_, KeyCode::Char(' ')) => app.toggle_detail(),
                        (_, KeyCode::Enter) => {
                            open_in_editor(app, terminal);
                        }
                        (_, KeyCode::Esc) => {
                            if app.show_detail {
                                app.show_detail = false;
                            } else if app.filtered_indices.is_some() {
                                app.clear_filter();
                            } else if !app.search_query.is_empty() {
                                app.search_query.clear();
                                app.search_matches.clear();
                                app.search_match_idx = None;
                            }
                        }

                        _ => {}
                    }
                }
                InputMode::Search | InputMode::Filter => {
                    match key.code {
                        KeyCode::Enter => {
                            let query = app.input_buf.clone();
                            if app.input_mode == InputMode::Search {
                                app.search_query = query;
                                app.do_search();
                                // Jump to first match
                                if let Some(idx) = app.search_match_idx {
                                    app.cursor = app.search_matches[idx];
                                    app.ensure_visible(log_height);
                                }
                            } else {
                                app.filter_query = query;
                                app.apply_filter();
                            }
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Esc => {
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Backspace => {
                            app.input_buf.pop();
                        }
                        KeyCode::Char(c) => {
                            app.input_buf.push(c);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pretty_print_simple() {
        let input = r#"{"a":1,"b":"hello"}"#;
        let result = pretty_print_json(input);
        assert_eq!(result, "{\n  \"a\": 1,\n  \"b\": \"hello\"\n}");
    }

    #[test]
    fn test_pretty_print_nested() {
        let input = r#"{"a":{"b":2}}"#;
        let result = pretty_print_json(input);
        assert_eq!(result, "{\n  \"a\": {\n    \"b\": 2\n  }\n}");
    }

    #[test]
    fn test_pretty_print_empty_object() {
        let input = r#"{"a":{},"b":1}"#;
        let result = pretty_print_json(input);
        assert_eq!(result, "{\n  \"a\": {},\n  \"b\": 1\n}");
    }

    #[test]
    fn test_pretty_print_array() {
        let input = r#"{"a":[1,2,3]}"#;
        let result = pretty_print_json(input);
        assert_eq!(result, "{\n  \"a\": [\n    1,\n    2,\n    3\n  ]\n}");
    }
}
