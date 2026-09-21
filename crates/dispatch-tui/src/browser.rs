//! Browsing the filesystem for a project to open.
//!
//! Dispatch is started against a directory, and the projects it keeps are the
//! ones it has been started against. This is the other way in: walk to a
//! directory, or type its path, and open it without restarting.

use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::sidebar::{REPOSITORY, SHUT_FOLDER};

/// One directory on offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Where it is.
    pub path: PathBuf,
    /// What it is called: its final path component.
    pub label: String,
    /// Whether it holds a `.git`, and so is a checkout rather than a plain
    /// directory.
    pub repo: bool,
}

/// How deep a scan looks for repositories.
///
/// Three levels reaches `~/code/org/project` and stops well short of walking a
/// home directory whole.
const SCAN_DEPTH: usize = 3;

/// How many repositories one scan will report.
///
/// A cap rather than a promise of completeness: a listing nobody can read to
/// the end is no more useful than a short one, and the walk has to finish
/// while the user is waiting for it.
const SCAN_LIMIT: usize = 500;

/// Directories a scan never descends into: they hold no projects and they are
/// where the time goes.
const SKIPPED: &[&str] = &["node_modules", "target", ".git"];

/// A directory, its subdirectories, and what the user has typed.
#[derive(Debug, Clone)]
pub struct Browser {
    /// The directory being listed.
    dir: PathBuf,
    /// Its subdirectories, in name order, before anything is typed.
    ///
    /// During a scan these are the repositories found beneath it instead.
    entries: Vec<Entry>,
    /// What the user has typed: a filter over `entries`, or a path when it
    /// carries a separator.
    input: String,
    /// Which of the *visible* entries the user is sitting on.
    selected: usize,
    /// Whether the listing is a scan's findings rather than one directory's
    /// own children.
    scanning: bool,
}

impl Browser {
    /// Opens the browser on `dir`.
    ///
    /// A directory that cannot be read lists nothing rather than failing:
    /// a permission error on one directory is not a reason to refuse to
    /// browse at all.
    #[must_use]
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            entries: read_dir(dir),
            input: String::new(),
            selected: 0,
            scanning: false,
        }
    }

    /// Whether the listing is a scan's findings.
    #[must_use]
    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    /// Whether what has been typed is a path rather than a filter.
    ///
    /// A separator is what tells them apart: no directory name contains one,
    /// so nothing that could be a filter is read as a path by mistake.
    #[must_use]
    pub fn is_path(&self) -> bool {
        self.input.contains('/') || self.input.starts_with('~') || self.input.contains('\\')
    }

    /// The directory being listed.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// What the user has typed.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    /// The entries on offer, after whatever has been typed.
    #[must_use]
    pub fn visible(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|entry| matches(entry, &self.input))
            .collect()
    }

    /// The entry the user is sitting on.
    #[must_use]
    pub fn selected(&self) -> Option<&Entry> {
        self.visible().get(self.selected).copied()
    }

    /// Types a character.
    pub fn push(&mut self, c: char) {
        self.input.push(c);
        self.selected = 0;
    }

    /// Takes the last character back.
    pub fn backspace(&mut self) {
        self.input.pop();
        self.selected = 0;
    }

    /// Moves down one entry, stopping at the last.
    ///
    /// Stops rather than wraps: a list you are walking to its end should end,
    /// not silently start again.
    pub fn next(&mut self) {
        let last = self.visible().len().saturating_sub(1);
        self.selected = (self.selected + 1).min(last);
    }

    /// Moves up one entry, stopping at the first.
    pub fn previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Lists the directory the user is sitting on.
    pub fn descend(&mut self) {
        let Some(path) = self.selected().map(|entry| entry.path.clone()) else {
            return;
        };

        self.open(path, None);
    }

    /// Lists the parent, sitting on the directory just left.
    ///
    /// Walking up and finding the cursor somewhere unrelated is how you lose
    /// your place in a deep tree.
    pub fn ascend(&mut self) {
        let Some(parent) = self.dir.parent().map(Path::to_path_buf) else {
            return;
        };

        let leaving = self.dir.clone();
        self.open(parent, Some(&leaving));
    }

    /// The typed path, when it names a directory that is there.
    #[must_use]
    pub fn typed_dir(&self) -> Option<PathBuf> {
        let path = expand(&self.input);
        path.is_dir().then_some(path)
    }

    /// Lists the typed path, if it is a directory.
    ///
    /// Answers whether it was. A path that is not there leaves everything as
    /// it was, with the text still typed: the fix is usually one character.
    pub fn jump(&mut self) -> bool {
        let path = expand(&self.input);

        if !path.is_dir() {
            return false;
        }

        self.open(path, None);
        true
    }

    /// Fills in the rest of a typed path, as far as it is unambiguous.
    pub fn complete(&mut self) {
        if !self.is_path() {
            return;
        }

        let typed = expand(&self.input);
        let (dir, prefix) = match typed.file_name() {
            Some(name) => (
                typed.parent().unwrap_or(Path::new("/")).to_path_buf(),
                name.to_string_lossy().into_owned(),
            ),
            None => (typed.clone(), String::new()),
        };

        let candidates: Vec<Entry> = read_dir(&dir)
            .into_iter()
            .filter(|entry| entry.label.starts_with(&prefix))
            .collect();

        let Some(only) = candidates.first() else {
            return;
        };
        if candidates.len() > 1 {
            return;
        }

        self.input = only.path.display().to_string();
        self.selected = 0;
    }

    /// Lists every repository beneath this directory instead of its own
    /// children, or goes back to listing its children.
    pub fn toggle_scan(&mut self) {
        self.scanning = !self.scanning;
        self.input.clear();
        self.selected = 0;

        self.entries = if self.scanning {
            scan(&self.dir)
        } else {
            read_dir(&self.dir)
        };
    }

    /// Lists `dir`, sitting on `sitting_on` when it is one of its entries.
    fn open(&mut self, dir: PathBuf, sitting_on: Option<&Path>) {
        self.entries = read_dir(&dir);
        self.dir = dir;
        // A filter belongs to the listing it filtered.
        self.input.clear();
        self.scanning = false;

        self.selected = sitting_on
            .and_then(|path| self.visible().iter().position(|entry| entry.path == path))
            .unwrap_or(0);
    }
}

/// A typed path with `~` replaced by the home directory.
fn expand(typed: &str) -> PathBuf {
    let Some(rest) = typed.strip_prefix('~') else {
        return PathBuf::from(typed);
    };

    let Some(home) = home_dir() else {
        return PathBuf::from(typed);
    };

    home.join(rest.trim_start_matches('/'))
}

/// The user's home directory, as the environment names it.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Every repository beneath `dir`, to [`SCAN_DEPTH`] levels.
///
/// The point of it: a directory of checkouts answers in one keystroke rather
/// than one descent per project.
fn scan(dir: &Path) -> Vec<Entry> {
    let mut found = Vec::new();
    let mut frontier = vec![(dir.to_path_buf(), 0usize)];

    while let Some((dir, depth)) = frontier.pop() {
        if depth > SCAN_DEPTH || found.len() >= SCAN_LIMIT {
            continue;
        }

        for entry in read_dir(&dir) {
            if found.len() >= SCAN_LIMIT {
                break;
            }
            if entry.label.starts_with('.') || SKIPPED.contains(&entry.label.as_str()) {
                continue;
            }

            if entry.repo {
                // A checkout's own subdirectories are its working tree, not
                // more projects, so the walk stops here.
                found.push(entry);
                continue;
            }

            frontier.push((entry.path, depth + 1));
        }
    }

    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// Whether `entry` survives `filter`.
///
/// A directory whose name starts with a dot is left out until the filter
/// starts with one too: browsing is for the projects a user keeps, and a home
/// directory's dotfiles bury them. Typing the dot is what asks for them.
fn matches(entry: &Entry, filter: &str) -> bool {
    if entry.label.starts_with('.') && !filter.starts_with('.') {
        return false;
    }

    if filter.is_empty() {
        return true;
    }

    entry.label.to_lowercase().contains(&filter.to_lowercase())
}

/// The subdirectories of `dir`, in name order.
///
/// Directories only: a file is never a project, so offering one would be a row
/// that cannot be chosen.
fn read_dir(dir: &Path) -> Vec<Entry> {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut entries: Vec<Entry> = listing
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            let path = entry.path();
            Entry {
                label: entry.file_name().to_string_lossy().into_owned(),
                repo: path.join(".git").exists(),
                path,
            }
        })
        .collect();

    entries.sort_by(|a, b| a.label.cmp(&b.label));
    entries
}

#[cfg(test)]
mod tests;

/// What the browser calls itself.
const TITLE: &str = " Open project ";

/// The keys it answers to, drawn under the listing.
const KEYS: &str = "→ in · ← up · ^g repos · ⏎ open";

/// Writes `text` at `(x, y)`, clipped to `area`.
fn write(buf: &mut Buffer, area: Rect, x: u16, y: u16, text: &str, style: Style) {
    for (cursor, grapheme) in (x..).zip(text.chars()) {
        if cursor >= area.x + area.width || y >= area.y + area.height {
            break;
        }
        if let Some(cell) = buf.cell_mut((cursor, y)) {
            cell.set_symbol(&grapheme.to_string());
            cell.set_style(style);
        }
    }
}

/// The rectangle of `width` by `height` at the middle of `area`.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 2,
        width.min(area.width),
        height.min(area.height),
    )
}

/// The last `width` columns of `text`, marked with an ellipsis when it is cut.
///
/// Cut from the front, unlike everything else Dispatch truncates: the end of a
/// path says which directory it is, and the start is what every path under one
/// home directory has in common.
fn tail(text: &str, width: usize) -> String {
    let length = text.chars().count();

    if length <= width || width <= 1 {
        return text.to_string();
    }

    let kept: String = text.chars().skip(length - (width - 1)).collect();
    format!("…{kept}")
}

/// A path with the home directory written as `~`, which is both shorter and
/// how the user thinks of it.
fn short(path: &Path) -> String {
    let text = path.display().to_string();

    let Some(home) = home_dir() else {
        return text;
    };
    let home = home.display().to_string();

    match text.strip_prefix(&home) {
        Some(rest) => format!("~{rest}"),
        None => text,
    }
}

impl Widget for &Browser {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 8 || area.height < 6 {
            return;
        }

        let width = (area.width * 2 / 3).clamp(30.min(area.width), area.width);
        let height = (area.height * 2 / 3).clamp(6, area.height);
        let rect = centred(area, width, height);

        // It floats over the grid, so whatever it covers is erased rather than
        // left showing through.
        Clear.render(rect, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(TITLE)
            .border_style(Style::default().fg(Color::Cyan));
        let inner = block.inner(rect);
        block.render(rect, buf);

        if inner.height < 3 {
            return;
        }

        // Where we are, then what has been typed, then the listing, then the
        // keys: the listing is the part that grows.
        // The word first and the path after it: cutting a path keeps its end,
        // and a cut that ate the word would leave the listing unexplained.
        let prefix = if self.scanning {
            "repositories under "
        } else {
            ""
        };
        let label = u16::try_from(prefix.chars().count()).unwrap_or(0);
        let room = inner.width.saturating_sub(label) as usize;

        let style = Style::default().fg(Color::Cyan);
        write(buf, inner, inner.x, inner.y, prefix, style);
        write(
            buf,
            inner,
            inner.x + label,
            inner.y,
            &tail(&short(&self.dir), room),
            style,
        );

        write(buf, inner, inner.x, inner.y + 1, "> ", Style::default());
        write(
            buf,
            inner,
            inner.x + 2,
            inner.y + 1,
            self.input(),
            Style::default().add_modifier(Modifier::BOLD),
        );

        let rows = inner.height.saturating_sub(3);
        let visible = self.visible();

        // The selected row stays on screen in a listing longer than the box.
        let first = (self.selected + 1).saturating_sub(rows as usize);

        for (offset, entry) in visible.iter().skip(first).take(rows as usize).enumerate() {
            let y = inner.y + 2 + offset as u16;
            let style = if first + offset == self.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };

            write(
                buf,
                inner,
                inner.x,
                y,
                &" ".repeat(inner.width as usize),
                style,
            );
            write(buf, inner, inner.x, y, SHUT_FOLDER, style);

            // A scan reports paths from all over the tree, so the label alone
            // would not say which "src" this is — and a path cut to fit keeps
            // its end, which is the part naming the project.
            let room = inner.width.saturating_sub(4) as usize;
            let label = if self.scanning {
                tail(&short(&entry.path), room)
            } else {
                entry.label.clone()
            };
            write(buf, inner, inner.x + 2, y, &label, style);

            if entry.repo {
                let x = inner.x + 3 + u16::try_from(label.chars().count()).unwrap_or(0);
                write(buf, inner, x, y, REPOSITORY, style);
            }
        }

        write(
            buf,
            inner,
            inner.x,
            inner.y + inner.height - 1,
            KEYS,
            Style::default().fg(Color::DarkGray),
        );
    }
}
