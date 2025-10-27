use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use strsim::levenshtein;

#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct Todo {
    text: String,
    completed: bool,
    #[serde(default)]
    priority: Option<char>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default, skip_serializing, skip_deserializing)]
    note_expanded: bool,
}

#[derive(Clone, PartialEq)]
enum Mode {
    Normal,
    Insert,
    Command,
    Visual,
    Search,
    NoteEdit,
    Help,
}

#[derive(Clone)]
enum Action {
    Toggle,
    Delete,
}

struct AppSnapshot {
    todos: Vec<Todo>,
    selected_index: Option<usize>,
}

struct App {
    todos: Vec<Todo>,
    filtered_todos: Vec<usize>,
    history: VecDeque<AppSnapshot>,
    history_index: usize,
    list_state: ListState,
    mode: Mode,
    input: String,
    command_input: String,
    message: String,
    visual_start: Option<usize>,
    search_query: String,
    note_input: String,
    current_note_index: Option<usize>,
    help_scroll: usize,
    is_dirty: bool,
    saved_snapshot: Option<Vec<Todo>>,
    is_editing: bool,
    repeat_count: usize,
    clipboard: Vec<Todo>,
    last_action: Option<Action>,
}

impl App {
    fn new() -> App {
        let mut state = ListState::default();
        state.select(Some(0));

        // Don't save initial snapshot - that way it won't be dirty on startup
        App {
            todos: vec![],
            filtered_todos: Vec::new(),
            history: VecDeque::new(),
            history_index: 0,
            list_state: state,
            mode: Mode::Normal,
            input: String::new(),
            command_input: String::new(),
            message: String::new(),
            visual_start: None,
            search_query: String::new(),
            note_input: String::new(),
            current_note_index: None,
            help_scroll: 0,
            is_dirty: false,
            saved_snapshot: None,
            is_editing: false,
            repeat_count: 0,
            clipboard: Vec::new(),
            last_action: None,
        }
    }

    fn get_todo_file() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".tuido.json")
    }

    fn load_todos(&mut self) {
        let file_path = Self::get_todo_file();
        if let Ok(contents) = fs::read_to_string(&file_path)
            && let Ok(todos) = serde_json::from_str::<Vec<Todo>>(&contents)
        {
            self.todos = todos.clone();
            self.filtered_todos = (0..self.todos.len()).collect();
            if !self.todos.is_empty() {
                self.list_state.select(Some(0));
            } else {
                self.list_state.select(None);
            }
            self.message = "Loaded todos from file".to_string();
            self.is_dirty = false;
            self.saved_snapshot = Some(todos);
            // Save initial snapshot for undo history
            self.save_snapshot();
        } else {
            // No file, so fresh start - this is clean
            self.filtered_todos = vec![];
            self.is_dirty = false;
            self.saved_snapshot = Some(vec![]);
        }
    }

    fn save_todos(&mut self) -> io::Result<()> {
        let file_path = Self::get_todo_file();
        let json = serde_json::to_string_pretty(&self.todos)?;
        fs::write(&file_path, json)?;
        self.is_dirty = false;
        self.saved_snapshot = Some(self.todos.clone());
        Ok(())
    }

    fn save_todos_to(&mut self, file_path: &str) -> io::Result<()> {
        let json = serde_json::to_string_pretty(&self.todos)?;
        fs::write(file_path, json)?;
        // Update snapshot to match, so is_dirty stays correct
        self.saved_snapshot = Some(self.todos.clone());
        Ok(())
    }

    fn export_todotxt(&self, file_path: &str) -> io::Result<()> {
        let mut output = Vec::new();
        for todo in &self.todos {
            if todo.completed {
                write!(output, "x ")?;
            }
            if let Some(priority) = todo.priority {
                write!(output, "({}) ", priority)?;
            }
            writeln!(output, "{}", todo.text)?;
        }
        fs::write(file_path, output)?;
        Ok(())
    }

    fn export_markdown(&self, file_path: &str) -> io::Result<()> {
        let mut output = Vec::new();
        writeln!(output, "# TODOs\n")?;
        for todo in &self.todos {
            let checkbox = if todo.completed { "[x]" } else { "[ ]" };
            writeln!(output, "- {} {}", checkbox, todo.text)?;
        }
        fs::write(file_path, output)?;
        Ok(())
    }

    fn save_snapshot(&mut self) {
        // Truncate forward history if we're not at the end
        if self.history_index < self.history.len() {
            self.history.truncate(self.history_index);
        }

        // Add new snapshot
        self.history.push_back(AppSnapshot {
            todos: self.todos.clone(),
            selected_index: self.list_state.selected(),
        });

        // Limit history size
        if self.history.len() > 100 {
            self.history.pop_front();
        }
        self.history_index = self.history.len();
        self.update_dirty_status();
    }

    fn update_dirty_status(&mut self) {
        if let Some(ref saved) = self.saved_snapshot {
            self.is_dirty = saved != &self.todos;
        } else {
            self.is_dirty = true;
        }
    }

    fn undo(&mut self) {
        if self.history_index > 0 {
            self.history_index -= 1;
            if let Some(snapshot) = self.history.get(self.history_index) {
                self.todos = snapshot.todos.clone();
                self.list_state.select(snapshot.selected_index);
                self.message = "Undo: reverted to previous state".to_string();
                self.filter_todos();
                self.update_dirty_status();
            }
        } else {
            self.message = "Nothing to undo".to_string();
        }
    }

    fn redo(&mut self) {
        if self.history_index < self.history.len() - 1 {
            self.history_index += 1;
            if let Some(snapshot) = self.history.get(self.history_index) {
                self.todos = snapshot.todos.clone();
                self.list_state.select(snapshot.selected_index);
                self.message = "Redo: reapplied change".to_string();
                self.filter_todos();
                self.update_dirty_status();
            }
        } else {
            self.message = "Nothing to redo".to_string();
        }
    }

    fn filter_todos(&mut self) {
        if self.search_query.is_empty() {
            self.filtered_todos = (0..self.todos.len()).collect();
        } else {
            let query = self.search_query.to_lowercase();
            let query_len = query.len();

            self.filtered_todos = self
                .todos
                .iter()
                .enumerate()
                .filter_map(|(i, todo)| {
                    let text = todo.text.to_lowercase();

                    // Exact match
                    if text.contains(&query) {
                        return Some(i);
                    }

                    // Check if query is a subsequence (e.g., "proj" matches "project")
                    // This is cheaper than Levenshtein for long texts
                    let mut query_chars = query.chars().peekable();
                    for c in text.chars() {
                        if query_chars.peek() == Some(&c) {
                            query_chars.next();
                            if query_chars.peek().is_none() {
                                // Subsequence match found
                                return Some(i);
                            }
                        }
                    }

                    // If subsequence check failed, try Levenshtein distance
                    let max_distance = if query_len <= 3 { 1 } else { 2 };
                    let distance = levenshtein(&query, &text);
                    if distance <= max_distance {
                        Some(i)
                    } else {
                        None
                    }
                })
                .collect();
        }

        // Adjust selection if out of bounds
        if let Some(selected) = self.list_state.selected()
            && selected >= self.filtered_todos.len()
        {
            if !self.filtered_todos.is_empty() {
                self.list_state.select(Some(self.filtered_todos.len() - 1));
            } else {
                self.list_state.select(None);
            }
        }
    }

    fn parse_priority(text: &str) -> (Option<char>, String) {
        if let Some(stripped) = text
            .strip_prefix("(A)")
            .or_else(|| text.strip_prefix("(a)"))
        {
            return (Some('A'), stripped.trim().to_string());
        } else if let Some(stripped) = text
            .strip_prefix("(B)")
            .or_else(|| text.strip_prefix("(b)"))
        {
            return (Some('B'), stripped.trim().to_string());
        } else if let Some(stripped) = text
            .strip_prefix("(C)")
            .or_else(|| text.strip_prefix("(c)"))
        {
            return (Some('C'), stripped.trim().to_string());
        }
        (None, text.to_string())
    }

    fn get_selected_indices(&self) -> Vec<usize> {
        match self.mode {
            Mode::Visual if self.visual_start.is_some() => {
                if let Some(start) = self.visual_start {
                    if let Some(current) = self.list_state.selected() {
                        let start_idx = start.min(current);
                        let end_idx = start.max(current);
                        (start_idx..=end_idx).collect()
                    } else {
                        vec![start]
                    }
                } else {
                    vec![]
                }
            }
            Mode::Normal => {
                if let Some(idx) = self.list_state.selected() {
                    vec![idx]
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }

    fn next(&mut self) {
        if self.filtered_todos.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) if i < self.filtered_todos.len() => {
                if i >= self.filtered_todos.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            _ => 0,
        };
        if i < self.filtered_todos.len() {
            self.list_state.select(Some(i));
        }
    }

    fn previous(&mut self) {
        if self.filtered_todos.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) if i < self.filtered_todos.len() => {
                if i == 0 {
                    self.filtered_todos.len() - 1
                } else {
                    i - 1
                }
            }
            _ => 0,
        };
        if i < self.filtered_todos.len() {
            self.list_state.select(Some(i));
        }
    }

    fn toggle_todo(&mut self) {
        let indices = self.get_selected_indices();
        if indices.is_empty() {
            return;
        }

        let count = indices.len();

        self.save_snapshot();

        for &idx in &indices {
            if let Some(i) = self.filtered_todos.get(idx)
                && *i < self.todos.len()
            {
                self.todos[*i].completed = !self.todos[*i].completed;
            }
        }

        self.message = if count == 1 {
            "TODO toggled".to_string()
        } else {
            format!("{} todos toggled", count)
        };

        // Track last action for repeat
        self.last_action = Some(Action::Toggle);

        if self.mode == Mode::Visual {
            self.mode = Mode::Normal;
            self.visual_start = None;
        }
    }

    fn delete_todo(&mut self) {
        let indices = self.get_selected_indices();
        if indices.is_empty() {
            return;
        }

        // Copy selected todos to clipboard before deleting
        self.clipboard = indices
            .iter()
            .filter_map(|&idx| self.filtered_todos.get(idx))
            .filter_map(|&i| self.todos.get(i).cloned())
            .collect();

        self.save_snapshot();

        // Collect actual indices in reverse order
        let mut to_delete: Vec<usize> = indices
            .iter()
            .filter_map(|&idx| self.filtered_todos.get(idx).copied())
            .collect();
        to_delete.sort();
        to_delete.reverse();

        // Delete in reverse order to maintain indices
        for idx in to_delete {
            if idx < self.todos.len() {
                self.todos.remove(idx);
            }
        }

        // Update filtered_todos
        self.filter_todos();

        // Adjust selection with better clamping for empty/shrinking lists
        if self.todos.is_empty() {
            // Completely empty list
            self.list_state.select(None);
        } else if self.filtered_todos.is_empty() {
            // All todos filtered out but some still exist
            self.list_state.select(None);
        } else {
            // Calculate safe selection index
            let first_deleted_idx = if let Some(&first_idx) = indices.first() {
                first_idx
            } else {
                0
            };

            // Clamp to valid range and prefer staying at same visual position
            let max_idx = self.filtered_todos.len().saturating_sub(1);
            let new_idx = first_deleted_idx.min(max_idx);

            if let Some(selected_idx) = new_idx.checked_sub(0) {
                if selected_idx < self.filtered_todos.len() {
                    self.list_state.select(Some(selected_idx));
                } else {
                    self.list_state.select(Some(max_idx));
                }
            } else {
                self.list_state.select(Some(0));
            }
        }

        self.message = if indices.len() == 1 {
            "TODO deleted".to_string()
        } else {
            format!("{} todos deleted", indices.len())
        };

        // Track last action for repeat
        self.last_action = Some(Action::Delete);

        if self.mode == Mode::Visual {
            self.mode = Mode::Normal;
            self.visual_start = None;
        }
    }

    fn add_todo(&mut self) {
        if self.input.trim().is_empty() {
            self.message = "Empty todo not added".to_string();
            return;
        }

        self.save_snapshot();

        let (priority, text) = Self::parse_priority(&self.input);

        let todo = Todo {
            text,
            completed: false,
            priority,
            note: None,
            note_expanded: false,
        };

        self.todos.push(todo);
        self.filter_todos();
        if !self.filtered_todos.is_empty() {
            self.list_state.select(Some(self.filtered_todos.len() - 1));
        }
        self.input.clear();
    }

    fn open_note_editor(&mut self) {
        if let Some(idx) = self.list_state.selected()
            && let Some(&todo_idx) = self.filtered_todos.get(idx)
            && todo_idx < self.todos.len()
        {
            self.current_note_index = Some(todo_idx);
            self.note_input = self.todos[todo_idx].note.clone().unwrap_or_default();
            self.mode = Mode::NoteEdit;
        }
    }

    fn save_note(&mut self) {
        if let Some(todo_idx) = self.current_note_index
            && todo_idx < self.todos.len()
        {
            self.save_snapshot();
            if self.note_input.trim().is_empty() {
                self.todos[todo_idx].note = None;
            } else {
                self.todos[todo_idx].note = Some(self.note_input.clone());
            }
            self.message = "Note saved".to_string();
        }
        self.mode = Mode::Normal;
        self.note_input.clear();
        self.current_note_index = None;
    }

    fn cancel_note(&mut self) {
        self.mode = Mode::Normal;
        self.note_input.clear();
        self.current_note_index = None;
    }

    fn show_help(&mut self) {
        self.mode = Mode::Help;
        self.help_scroll = 0;
    }

    fn hide_help(&mut self) {
        self.mode = Mode::Normal;
    }

    fn edit_todo(&mut self) {
        if let Some(idx) = self.list_state.selected()
            && let Some(&todo_idx) = self.filtered_todos.get(idx)
            && todo_idx < self.todos.len()
        {
            self.input = self.todos[todo_idx].text.clone();
            self.mode = Mode::Insert;
            self.list_state.select(Some(idx));
            self.is_editing = true;
        }
    }

    fn save_edited_todo(&mut self) {
        if self.input.trim().is_empty() {
            // Empty text means delete the todo (vim-like behavior)
            if let Some(idx) = self.list_state.selected()
                && let Some(&todo_idx) = self.filtered_todos.get(idx)
                && todo_idx < self.todos.len()
            {
                self.save_snapshot();
                self.todos.remove(todo_idx);
                self.message = "TODO deleted (empty text)".to_string();
                self.filter_todos();

                // Adjust selection after deletion
                if !self.filtered_todos.is_empty() {
                    let new_idx = idx.min(self.filtered_todos.len() - 1);
                    self.list_state.select(Some(new_idx));
                } else {
                    self.list_state.select(None);
                }
            }
            self.mode = Mode::Normal;
            self.input.clear();
            self.is_editing = false;
            return;
        }

        if let Some(idx) = self.list_state.selected()
            && let Some(&todo_idx) = self.filtered_todos.get(idx)
            && todo_idx < self.todos.len()
        {
            self.save_snapshot();
            let (priority, text) = Self::parse_priority(&self.input);
            self.todos[todo_idx].text = text;
            self.todos[todo_idx].priority = priority;
            self.message = "TODO updated".to_string();
        }
        self.mode = Mode::Normal;
        self.input.clear();
        self.is_editing = false;
    }

    fn yank_todo(&mut self) {
        let indices = self.get_selected_indices();
        if indices.is_empty() {
            return;
        }

        // Copy selected todos to clipboard
        self.clipboard = indices
            .iter()
            .filter_map(|&idx| self.filtered_todos.get(idx))
            .filter_map(|&i| self.todos.get(i).cloned())
            .collect();

        self.message = if indices.len() == 1 {
            "TODO yanked".to_string()
        } else {
            format!("{} todos yanked", indices.len())
        };

        if self.mode == Mode::Visual {
            self.mode = Mode::Normal;
            self.visual_start = None;
        }
    }

    fn paste_todo(&mut self) {
        if self.clipboard.is_empty() {
            self.message = "Nothing to paste".to_string();
            return;
        }

        self.save_snapshot();

        // Determine insertion point
        let insert_pos = self
            .list_state
            .selected()
            .and_then(|i| self.filtered_todos.get(i))
            .map(|&i| i + 1)
            .unwrap_or(self.todos.len());

        // Insert todos in reverse order to maintain correct positions
        for todo in self.clipboard.iter().rev() {
            self.todos.insert(insert_pos, todo.clone());
        }

        self.filter_todos();
        self.message = format!("Pasted {} todos", self.clipboard.len());
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.load_todos();
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

fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    mut app: App,
) -> io::Result<()> {
    let mut last_key = ' ';
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if let Event::Key(key) = event::read()? {
            // Only process key press events, not release
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match app.mode {
                Mode::Normal => match key.code {
                    KeyCode::Char(c @ '1'..='9') => {
                        app.repeat_count = app.repeat_count * 10 + (c as usize - '0' as usize);
                    }
                    KeyCode::Char('q') => {
                        if app.is_dirty {
                            app.message = "Error: unsaved changes. Use :q! to quit without saving"
                                .to_string();
                        } else {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('j') => {
                        let count = app.repeat_count.max(1);
                        for _ in 0..count {
                            app.next();
                        }
                        app.repeat_count = 0;
                    }
                    KeyCode::Char('k') => {
                        let count = app.repeat_count.max(1);
                        for _ in 0..count {
                            app.previous();
                        }
                        app.repeat_count = 0;
                    }
                    KeyCode::Char('G') => {
                        if !app.filtered_todos.is_empty() {
                            app.list_state.select(Some(app.filtered_todos.len() - 1));
                        }
                    }
                    KeyCode::Char('g') => {
                        if last_key == 'g' && !app.filtered_todos.is_empty() {
                            app.list_state.select(Some(0));
                        }
                    }
                    KeyCode::Char('0') => {
                        // Only jump when not building a number (e.g., "10j")
                        if app.repeat_count == 0 && !app.filtered_todos.is_empty() {
                            app.list_state.select(Some(0));
                        }
                    }
                    KeyCode::Char('$') => {
                        if !app.filtered_todos.is_empty() {
                            app.list_state.select(Some(app.filtered_todos.len() - 1));
                        }
                    }
                    KeyCode::Char('x') => app.toggle_todo(),
                    KeyCode::Char('d') => {
                        if last_key == 'd' {
                            app.delete_todo();
                        }
                    }
                    KeyCode::Char('i') => {
                        app.mode = Mode::Insert;
                        app.input.clear();
                        app.message.clear();
                        app.is_editing = false;
                        app.repeat_count = 0;
                    }
                    KeyCode::Char('A') => {
                        app.mode = Mode::Insert;
                        app.input.clear();
                        app.message.clear();
                        app.is_editing = false;
                        app.repeat_count = 0;
                    }
                    KeyCode::Char('e') => app.edit_todo(),
                    KeyCode::Char(':') => {
                        app.mode = Mode::Command;
                        app.command_input.clear();
                        app.repeat_count = 0;
                    }
                    KeyCode::Char('v') => {
                        // Sync visual_start with current selection to avoid stale indices
                        app.visual_start = app.list_state.selected();
                        app.mode = Mode::Visual;
                        app.repeat_count = 0;
                    }
                    KeyCode::Char('y') => {
                        app.yank_todo();
                    }
                    KeyCode::Char('p') => {
                        app.paste_todo();
                    }
                    KeyCode::Char('/') => {
                        app.mode = Mode::Search;
                        app.search_query.clear();
                        app.repeat_count = 0;
                    }
                    KeyCode::Char('o') => app.open_note_editor(),
                    KeyCode::Char('u') => app.undo(),
                    KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.redo()
                    }
                    KeyCode::Char('?') => app.show_help(),
                    KeyCode::Char('.') => {
                        if let Some(action) = app.last_action.clone() {
                            match action {
                                Action::Toggle => app.toggle_todo(),
                                Action::Delete => app.delete_todo(),
                            }
                        }
                    }
                    KeyCode::Down => app.next(),
                    KeyCode::Up => app.previous(),
                    _ => {}
                },
                Mode::Insert => match key.code {
                    KeyCode::Esc => {
                        app.mode = Mode::Normal;
                        if !app.input.is_empty() {
                            if app.is_editing {
                                app.save_edited_todo();
                            } else {
                                app.add_todo();
                                app.message = "TODO added".to_string();
                            }
                        }
                        app.input.clear();
                        app.is_editing = false;
                    }
                    KeyCode::Char(c) => {
                        app.input.push(c);
                    }
                    KeyCode::Backspace => {
                        app.input.pop();
                    }
                    KeyCode::Enter => {
                        if app.is_editing {
                            app.save_edited_todo();
                        } else {
                            app.add_todo();
                            app.message = "TODO added".to_string();
                        }
                    }
                    _ => {}
                },
                Mode::Command => match key.code {
                    KeyCode::Esc => {
                        app.mode = Mode::Normal;
                        app.command_input.clear();
                    }
                    KeyCode::Char(c) => {
                        app.command_input.push(c);
                    }
                    KeyCode::Backspace => {
                        app.command_input.pop();
                    }
                    KeyCode::Enter => {
                        let cmd = app.command_input.trim().to_lowercase();
                        let parts: Vec<&str> = cmd.split_whitespace().collect();

                        match parts.as_slice() {
                            ["q" | "quit"] => {
                                if app.is_dirty {
                                    app.message =
                                        "Error: unsaved changes. Use :q! to quit without saving"
                                            .to_string();
                                } else {
                                    return Ok(());
                                }
                            }
                            ["q!"] => return Ok(()),
                            ["w"] => match app.save_todos() {
                                Ok(_) => app.message = "Saved to ~/.tuido.json".to_string(),
                                Err(e) => {
                                    app.message = format!("Error saving: {} (check permissions)", e)
                                }
                            },
                            ["wq"] => {
                                let _ = app.save_todos();
                                return Ok(());
                            }
                            ["clear"] => {
                                app.save_snapshot();
                                let len = app.todos.len();
                                app.todos.retain(|t| !t.completed);
                                let removed = len - app.todos.len();
                                app.filter_todos();
                                app.message = format!("Removed {} completed todos", removed);
                            }
                            ["sort"] => {
                                app.save_snapshot();
                                app.todos.sort_by(|a, b| b.completed.cmp(&a.completed));
                                app.message = "Sorted by completion status".to_string();
                            }
                            ["sort", "priority"] => {
                                app.save_snapshot();
                                app.todos.sort_by(|a, b| match (&b.priority, &a.priority) {
                                    (Some(b_pri), Some(a_pri)) => b_pri.cmp(a_pri),
                                    (Some(_), None) => std::cmp::Ordering::Less,
                                    (None, Some(_)) => std::cmp::Ordering::Greater,
                                    (None, None) => b.completed.cmp(&a.completed),
                                });
                                app.message = "Sorted by priority".to_string();
                            }
                            ["!", cmd @ ..] => {
                                let cmd_str = cmd.join(" ");
                                let output = if cfg!(target_os = "windows") {
                                    Command::new("cmd").args(["/C", &cmd_str]).output()
                                } else {
                                    Command::new("sh").args(["-c", &cmd_str]).output()
                                };

                                match output {
                                    Ok(output) => {
                                        let stdout = String::from_utf8_lossy(&output.stdout);
                                        app.message = format!("> {}", stdout.trim());
                                    }
                                    Err(e) => app.message = format!("Error: {}", e),
                                }
                            }
                            ["write", rest @ ..] if !rest.is_empty() => {
                                let file = rest.join(" ");
                                match app.save_todos_to(&file) {
                                    Ok(_) => {
                                        app.is_dirty = false;
                                        app.message = format!("Saved to {}", file);
                                    }
                                    Err(e) => {
                                        app.message = format!(
                                            "Error saving to {}: {} (check permissions/path)",
                                            file, e
                                        );
                                    }
                                }
                            }
                            ["write"] => {
                                // Fallback: save to default file if no filename given
                                match app.save_todos() {
                                    Ok(_) => app.message = "Saved to ~/.tuido.json".to_string(),
                                    Err(e) => {
                                        app.message =
                                            format!("Error saving: {} (check permissions)", e)
                                    }
                                }
                            }
                            ["open", rest @ ..] if !rest.is_empty() => {
                                let file = rest.join(" ");
                                match fs::read_to_string(&file) {
                                    Ok(contents) => {
                                        match serde_json::from_str::<Vec<Todo>>(&contents) {
                                            Ok(todos) => {
                                                app.save_snapshot();
                                                app.todos = todos;
                                                app.filter_todos();
                                                app.message = format!("Loaded from {}", file);
                                            }
                                            Err(_) => {
                                                app.message =
                                                    format!("Invalid file format in {}", file)
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        app.message = format!(
                                            "Error opening {}: {} (file not found?)",
                                            file, e
                                        )
                                    }
                                }
                            }
                            ["open"] => {
                                app.message =
                                    "Usage: :open <filename> (use quotes for spaces)".to_string();
                            }
                            ["export", rest @ ..] if !rest.is_empty() => {
                                let file = rest.join(" ");
                                if file.ends_with(".txt") {
                                    match app.export_todotxt(&file) {
                                        Ok(_) => app.message = format!("Exported to {}", file),
                                        Err(e) => app.message = format!("Error: {}", e),
                                    }
                                } else if file.ends_with(".md") {
                                    match app.export_markdown(&file) {
                                        Ok(_) => app.message = format!("Exported to {}", file),
                                        Err(e) => app.message = format!("Error: {}", e),
                                    }
                                } else {
                                    app.message =
                                        format!("Unsupported format: {} (use .txt or .md)", file);
                                }
                            }
                            ["export"] => {
                                app.message = "Usage: :export <filename> (use quotes for spaces, .txt or .md)".to_string();
                            }
                            ["help"] => {
                                app.command_input.clear();
                                app.mode = Mode::Help;
                                app.help_scroll = 0;
                                continue; // Skip setting mode back to Normal
                            }
                            _ => {
                                app.message = format!("Unknown command: {}", app.command_input);
                            }
                        }
                        app.mode = Mode::Normal;
                        app.command_input.clear();
                    }
                    _ => {}
                },
                Mode::Visual => match key.code {
                    KeyCode::Esc => {
                        app.mode = Mode::Normal;
                        app.visual_start = None;
                    }
                    KeyCode::Char('j') => app.next(),
                    KeyCode::Char('k') => app.previous(),
                    KeyCode::Char('x') => app.toggle_todo(),
                    KeyCode::Char('d') => app.delete_todo(),
                    KeyCode::Char('y') => app.yank_todo(),
                    KeyCode::Down => app.next(),
                    KeyCode::Up => app.previous(),
                    _ => {}
                },
                Mode::Search => match key.code {
                    KeyCode::Esc => {
                        app.search_query.clear();
                        app.filter_todos();
                        app.mode = Mode::Normal;
                    }
                    KeyCode::Char(c) => {
                        app.search_query.push(c);
                        app.filter_todos();
                    }
                    KeyCode::Backspace => {
                        app.search_query.pop();
                        app.filter_todos();
                    }
                    KeyCode::Enter => {
                        app.mode = Mode::Normal;
                    }
                    _ => {}
                },
                Mode::NoteEdit => match key.code {
                    KeyCode::Esc => app.cancel_note(),
                    KeyCode::Enter => app.save_note(),
                    KeyCode::Char(c) => {
                        app.note_input.push(c);
                    }
                    KeyCode::Backspace => {
                        app.note_input.pop();
                    }
                    _ => {}
                },
                Mode::Help => match key.code {
                    KeyCode::Esc => app.hide_help(),
                    KeyCode::Char('j') => {
                        app.help_scroll = app.help_scroll.saturating_add(1);
                    }
                    KeyCode::Char('k') => {
                        app.help_scroll = app.help_scroll.saturating_sub(1);
                    }
                    KeyCode::Up => {
                        app.help_scroll = app.help_scroll.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        app.help_scroll = app.help_scroll.saturating_add(1);
                    }
                    _ => {}
                },
            }

            last_key = match key.code {
                KeyCode::Char(c) => c,
                _ => ' ',
            };
        }
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    match app.mode {
        Mode::Help => {
            render_help_popup(f, app);
        }
        _ => {
            render_main_ui(f, app);
        }
    }
}

fn render_main_ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    // Determine visual selection range if in visual mode
    let visual_range = if app.mode == Mode::Visual
        && app.visual_start.is_some()
        && let Some(start) = app.visual_start
        && let Some(end) = app.list_state.selected()
    {
        Some((start.min(end), start.max(end)))
    } else {
        None
    };

    // Main todo list
    let items: Vec<ListItem> = app
        .filtered_todos
        .iter()
        .enumerate()
        .map(|(idx, &todo_idx)| {
            let todo = &app.todos[todo_idx];
            let checkbox = if todo.completed { "[✓]" } else { "[ ]" };

            // Determine style based on completion and priority
            let mut style = if todo.completed {
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::CROSSED_OUT)
            } else {
                Style::default().fg(Color::White)
            };

            // Apply priority color if not completed
            if !todo.completed
                && let Some(priority) = todo.priority
            {
                style = style.add_modifier(Modifier::BOLD);
                match priority {
                    'A' => style = style.fg(Color::Red),
                    'B' => style = style.fg(Color::Yellow),
                    'C' => style = style.fg(Color::Blue),
                    _ => {}
                }
            }

            // Apply visual mode highlighting for selected items
            if let Some((start, end)) = visual_range
                && idx >= start
                && idx <= end
            {
                style = style.bg(Color::Rgb(40, 60, 80));
            }

            let note_indicator = if todo.note.is_some() && !todo.note_expanded {
                " ›"
            } else {
                ""
            };

            let content = format!(" {} {}{}", checkbox, todo.text, note_indicator);
            ListItem::new(content).style(style)
        })
        .collect();

    let list_block = Block::default().borders(Borders::ALL).title(" TODOs ");

    // Update highlight style based on visual mode
    let highlight_style = if app.mode == Mode::Visual {
        Style::default()
            .bg(Color::Rgb(40, 60, 80))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .bg(Color::Rgb(60, 60, 60))
            .add_modifier(Modifier::BOLD)
    };

    let list = List::new(items)
        .block(list_block)
        .highlight_style(highlight_style)
        .highlight_symbol("❯ ");

    f.render_stateful_widget(list, chunks[0], &mut app.list_state);

    // Status line (like vim's statusline)
    let mode_str = match app.mode {
        Mode::Normal => "-- NORMAL --",
        Mode::Insert => "-- INSERT --",
        Mode::Command => "-- COMMAND --",
        Mode::Visual => "-- VISUAL --",
        Mode::Search => "-- SEARCH --",
        Mode::NoteEdit => "-- NOTE EDIT --",
        Mode::Help => "-- HELP --",
    };

    let mode_color = match app.mode {
        Mode::Normal => Color::Cyan,
        Mode::Insert => Color::Green,
        Mode::Command => Color::Yellow,
        Mode::Visual => Color::Magenta,
        Mode::Search => Color::Blue,
        Mode::NoteEdit => Color::Cyan,
        Mode::Help => Color::White,
    };

    // Calculate stats
    let total = app.todos.len();
    let completed = app.todos.iter().filter(|t| t.completed).count();
    let percent = if total > 0 {
        (completed * 100) / total
    } else {
        0
    };
    let selected_idx = app.list_state.selected().map(|i| i + 1).unwrap_or(0);

    // Count priorities
    let priority_counts = app
        .todos
        .iter()
        .filter(|t| !t.completed && t.priority.is_some())
        .fold((0, 0, 0), |(a, b, c), t| match t.priority {
            Some('A') => (a + 1, b, c),
            Some('B') => (a, b + 1, c),
            Some('C') => (a, b, c + 1),
            _ => (a, b, c),
        });

    let mut status_parts = vec![
        Span::styled(
            format!(" {} ", mode_str),
            Style::default()
                .fg(Color::Black)
                .bg(mode_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" [{}/{}] {}% ", selected_idx, total, percent)),
        Span::raw(format!("│ {} completed ", completed)),
    ];

    // Add priority counts if any
    if priority_counts.0 + priority_counts.1 + priority_counts.2 > 0 {
        status_parts.push(Span::raw(format!(
            "│ A:{} B:{} C:{} ",
            priority_counts.0, priority_counts.1, priority_counts.2
        )));
    }

    // Add search results if in search mode or filtered
    if !app.search_query.is_empty() {
        status_parts.push(Span::styled(
            format!(
                "│ {} results for '{}'",
                app.filtered_todos.len(),
                app.search_query
            ),
            Style::default().fg(Color::Cyan),
        ));
    }

    let status_line = Paragraph::new(Line::from(status_parts))
        .style(Style::default().bg(Color::Rgb(30, 30, 30)).fg(Color::White));

    f.render_widget(status_line, chunks[1]);

    // Command line
    let cmd_line = match app.mode {
        Mode::Insert => Paragraph::new(format!("New TODO: {}", app.input))
            .style(Style::default().fg(Color::White)),
        Mode::Command => Paragraph::new(format!(":{}", app.command_input))
            .style(Style::default().fg(Color::White)),
        Mode::Search => {
            Paragraph::new(format!("/{}", app.search_query)).style(Style::default().fg(Color::Cyan))
        }
        Mode::NoteEdit => Paragraph::new(format!("Note: {}", app.note_input))
            .style(Style::default().fg(Color::White)),
        _ => Paragraph::new(app.message.clone()).style(Style::default().fg(Color::Yellow)),
    };

    f.render_widget(cmd_line, chunks[2]);
}

fn render_help_popup(f: &mut Frame, app: &mut App) {
    let help_text = vec![
        "",
        "KEY BINDINGS",
        "",
        "Navigation:",
        "  j / k          Move up/down",
        "  gg             Go to first todo",
        "  G              Go to last todo",
        "  0 / $          Jump to first/last",
        "  3j / 5k        Repeat motion N times",
        "",
        "Editing:",
        "  i              Insert new todo",
        "  A              Append new todo",
        "  e              Edit selected todo",
        "  x              Toggle completion",
        "  dd             Delete todo",
        "  o              Open note editor",
        "",
        "Yank/Paste:",
        "  y              Yank (copy) todo(s)",
        "  p              Paste below current",
        "  .              Repeat last action",
        "",
        "Undo/Redo:",
        "  u              Undo",
        "  Ctrl+r         Redo",
        "",
        "Visual Mode:",
        "  v              Enter visual mode",
        "  j / k          Extend selection",
        "  x              Toggle selected todos",
        "  d              Delete selected todos",
        "  Esc            Exit visual mode",
        "",
        "Search:",
        "  /              Start search",
        "  Enter          Confirm search",
        "  Esc            Clear search",
        "",
        "Commands:",
        "  :q             Quit (warns if unsaved)",
        "  :q!            Force quit without saving",
        "  :w             Save",
        "  :wq            Save and quit",
        "  :clear         Remove completed todos",
        "  :sort          Sort by completion",
        "  :sort priority Sort by priority",
        "  :!cmd          Execute shell command",
        "  :write <file>  Save to file (use quotes for spaces)",
        "  :open <file>   Load from file (use quotes for spaces)",
        "  :export <file> Export to .txt or .md (use quotes)",
        "  :help          Show this help",
        "",
        "Other:",
        "  ?              Show help",
        "  Esc            Exit current mode",
        "",
        "Press Esc to close",
    ];

    let items: Vec<ListItem> = help_text.into_iter().map(ListItem::new).collect();

    // Apply scroll offset - clamp to prevent OOB
    let max_offset = items.len().saturating_sub(1);
    let scroll_offset = app.help_scroll.min(max_offset);

    // Use terminal height minus border/status lines (approx 3 lines overhead)
    let available_height = (f.area().height.saturating_sub(3)) as usize;
    let take_lines = available_height.max(1);

    let visible_items: Vec<ListItem> = items
        .into_iter()
        .skip(scroll_offset)
        .take(take_lines)
        .collect();

    let list = List::new(visible_items)
        .block(Block::default().borders(Borders::ALL).title(" Help "))
        .style(Style::default().fg(Color::White));

    let area = centered_rect(60, 50, f.area());
    f.render_widget(list, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
