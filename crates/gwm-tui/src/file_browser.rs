//! A minimal host-filesystem browser for choosing a file to insert into an
//! image. Directories are listed first, then files; typing filters the current
//! directory. Hidden entries (dotfiles) are omitted.

use std::path::PathBuf;

use ratatui::widgets::ListState;

pub struct FsEntry {
    pub name: String,
    pub is_dir: bool,
}

pub struct FileBrowser {
    pub dir: PathBuf,
    pub entries: Vec<FsEntry>,
    pub state: ListState,
    pub filter: String,
}

impl FileBrowser {
    pub fn new(start: PathBuf) -> Self {
        let mut browser = Self {
            dir: start,
            entries: Vec::new(),
            state: ListState::default(),
            filter: String::new(),
        };
        browser.reload();
        browser
    }

    fn reload(&mut self) {
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        if let Ok(read) = std::fs::read_dir(&self.dir) {
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                if entry.path().is_dir() {
                    dirs.push(name);
                } else {
                    files.push(name);
                }
            }
        }
        dirs.sort_by_key(|s| s.to_lowercase());
        files.sort_by_key(|s| s.to_lowercase());
        self.entries = dirs
            .into_iter()
            .map(|name| FsEntry { name, is_dir: true })
            .chain(files.into_iter().map(|name| FsEntry { name, is_dir: false }))
            .collect();
        self.filter.clear();
        self.select_first();
    }

    pub fn filtered(&self) -> Vec<&FsEntry> {
        let needle = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|e| needle.is_empty() || e.name.to_lowercase().contains(&needle))
            .collect()
    }

    fn select_first(&mut self) {
        let has_any = !self.filtered().is_empty();
        self.state.select(has_any.then_some(0));
    }

    pub fn move_sel(&mut self, delta: isize) {
        let len = self.filtered().len();
        if len == 0 {
            self.state.select(None);
            return;
        }
        let current = self.state.selected().unwrap_or(0) as isize;
        self.state
            .select(Some((current + delta).rem_euclid(len as isize) as usize));
    }

    pub fn push_filter(&mut self, c: char) {
        self.filter.push(c);
        self.select_first();
    }

    pub fn pop_filter(&mut self) {
        self.filter.pop();
        self.select_first();
    }

    pub fn go_up(&mut self) {
        if let Some(parent) = self.dir.parent() {
            self.dir = parent.to_path_buf();
            self.reload();
        }
    }

    /// Act on the selection: descend into a directory (returns `None`) or choose
    /// a file (returns its path).
    pub fn enter(&mut self) -> Option<PathBuf> {
        let (name, is_dir) = self.state.selected().and_then(|i| {
            self.filtered().get(i).map(|e| (e.name.clone(), e.is_dir))
        })?;
        let path = self.dir.join(name);
        if is_dir {
            self.dir = path;
            self.reload();
            None
        } else {
            Some(path)
        }
    }
}
