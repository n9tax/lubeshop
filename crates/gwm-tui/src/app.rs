//! Application state and the event loop.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;
use ratatui::DefaultTerminal;

use gwm_core::archive::{RemoteFile, SearchHit};
use gwm_core::convert;
use gwm_core::formats;
use gwm_core::imagefs::{CreateOption, FileEntry, FsKind, FsUsage, ImageFs};
use gwm_core::library::{check_integrity, Integrity};
use gwm_core::models::{MediaItem, MediaKind, NewMediaItem, Source};
use gwm_core::Core;

use crate::count_job::{CountJob, CountState};
use crate::gotek_job::GotekJob;
use crate::version_job::{VersionJob, VersionState};
use gwm_core::convert::GotekFormat;
use gwm_core::usb::UsbDrive;
use crate::download_job::{DlOutcome, DownloadJob};
use crate::file_browser::FileBrowser;
use crate::install_job::InstallJob;
use crate::net_job::{NetJob, NetRequest, NetResult};
use crate::read_job::ReadJob;
use crate::rpm_job::RpmJob;
use crate::text_input::TextInput;
use crate::theme::{self, Theme};
use crate::write_job::WriteJob;

pub const MENU_ITEMS: [&str; 11] = [
    "Read a disk",
    "Write a disk",
    "Reset the device",
    "Test drive RPM",
    "Clean drive",
    "Library",
    "New image",
    "Import from archive.org",
    "Tools",
    "Settings",
    "Quit",
];

/// Rows on the settings screen.
pub const SETTINGS_ROWS: usize = 4;

/// A tunable Greaseweazle drive-delay parameter, matching a `gw delays --<name>`.
pub struct TuneParam {
    pub name: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub step: u32,
    pub max: u32,
    pub default: u32,
}

pub const TUNE_PARAMS: &[TuneParam] = &[
    TuneParam { name: "step", label: "Step delay", unit: "µs", step: 1000, max: 60000, default: 10000 },
    TuneParam { name: "settle", label: "Settle time", unit: "ms", step: 5, max: 500, default: 15 },
    TuneParam { name: "motor", label: "Motor delay", unit: "ms", step: 50, max: 3000, default: 750 },
    TuneParam { name: "select", label: "Select delay", unit: "µs", step: 5, max: 5000, default: 10 },
    TuneParam { name: "watchdog", label: "Watchdog", unit: "ms", step: 1000, max: 60000, default: 10000 },
    TuneParam { name: "pre-write", label: "Pre-write", unit: "µs", step: 50, max: 5000, default: 100 },
    TuneParam { name: "post-write", label: "Post-write", unit: "µs", step: 100, max: 10000, default: 1000 },
    TuneParam { name: "index-mask", label: "Index mask", unit: "µs", step: 50, max: 5000, default: 200 },
];

/// Drive selectors offered by the picker. `a` is first because the reference rig
/// answers to the twisted-cable end; unit numbers remain available.
pub const DRIVE_OPTIONS: [(&str, &str); 5] = [
    ("a", "Drive A   (twisted-cable end)"),
    ("b", "Drive B"),
    ("0", "Unit 0"),
    ("1", "Unit 1"),
    ("2", "Unit 2"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Menu,
    Library,
    LibraryConfirmDelete,
    LibraryRename,
    LibraryMove,
    EditNotes,
    NewFolder,
    FormatPicker,
    DrivePicker,
    NameInput,
    ReadOptions,
    Reading,
    ReadDone,
    WriteSource,
    WriteConfirm,
    Writing,
    WriteDone,
    Settings,
    Browse,
    BrowseInput,
    BrowseConfirmDelete,
    FileBrowse,
    HexView,
    TextEdit,
    NewImageName,
    Tools,
    ToolConfirm,
    Installing,
    DriverPicker,
    OptionPicker,
    DriveTuning,
    TuningSaveName,
    TuningProfiles,
    ArchiveSearch,
    ArchiveFetching,
    ArchiveResults,
    ArchiveFiles,
    ArchiveDownloading,
    GotekFormat,
    GotekDrive,
    GotekName,
    GotekSending,
    GotekDone,
}

/// Whether the shared driver/option pickers are serving a browse or a create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickMode {
    Browse,
    Create,
}

/// The Tools-menu index of the external tool a driver needs, so an uninstalled
/// driver can jump straight to it. `None` for TRS-80 (decoded in-crate, no tool).
fn tool_index_for_driver(driver: FsKind) -> Option<usize> {
    let cmd = match driver {
        FsKind::Cpm => "cpmls",
        FsKind::Fat => "mdir",
        FsKind::Cbm => "c1541",
        FsKind::Amiga => "xdftool",
        FsKind::Apple => "applecommander-ac",
        FsKind::Trs => return None,
    };
    gwm_core::tools::TOOLS.iter().position(|t| t.cmd == cmd)
}

/// What the host file browser is picking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileBrowseMode {
    /// Choose a file to insert into an image.
    InsertFile,
    /// Choose a directory (for the storage-folder setting).
    PickDir,
}

/// Which wizard the shared format/drive pickers are currently serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Read,
    Write,
    /// Choosing a `gw` disk format so a flux master can be decoded for browsing.
    Decode,
}

/// Which pane of the two-pane image browser has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Files,
    Clip,
}

/// A file staged in the cross-image copy clipboard (an extracted temp file).
#[derive(Debug, Clone)]
pub struct ClipItem {
    pub name: String,
    pub path: PathBuf,
}

/// A row in the folder-aware Library view.
#[derive(Debug, Clone)]
pub enum LibRow {
    /// `..` — go up a folder.
    Parent,
    /// A sub-folder in the current directory.
    Folder(String),
    /// A catalogued media file (by catalog id) in the current directory.
    File(i64),
}

pub struct App {
    pub core: Core,
    pub screen: Screen,
    pub theme: Theme,
    pub notice: Option<String>,
    should_quit: bool,
    flow: Flow,

    pub settings_index: usize,
    pub settings_editing: bool,
    pub storage_input: TextInput,
    pub tune_index: usize,
    pub tune_values: Vec<u32>,
    /// Name being typed when saving the current timings as a profile.
    pub tuning_name_input: TextInput,
    /// Selected row in the recall-profile picker (0 = "Default (factory)").
    pub tuning_profile_index: usize,

    pub menu_index: usize,

    pub library: Vec<MediaItem>,
    pub lib_state: ListState,
    pub lib_filter: String,
    pub lib_filtering: bool,
    pub lib_subpath: PathBuf,
    pub rename_input: TextInput,
    /// Folder-move picker state: candidate destination folders, the (scrolling)
    /// cursor, and the (id, old path, file name) of the item being moved.
    pub move_targets: Vec<PathBuf>,
    pub move_state: ListState,
    move_item: Option<(i64, String, String)>,
    pub notes_input: TextInput,
    notes_id: i64,
    pub folder_input: TextInput,
    pub delete_file: bool,
    pub verify_results: HashMap<i64, Integrity>,

    pub formats: Vec<String>,
    pub format_filter: String,
    pub format_state: ListState,
    /// When editing the selected format's label, the in-progress text (and the
    /// format id it applies to). `None` = not editing.
    pub format_editing: Option<(String, TextInput)>,

    pub drive_index: usize,
    pub name_input: TextInput,
    pub read_hard_sectors: bool,
    /// Read-options screen: selected row, start/end cylinder overrides (`None` =
    /// format default), and double-step for a 48 TPI disk in a 96 TPI drive.
    pub read_opt_row: usize,
    pub read_track_start: Option<u32>,
    pub read_track_end: Option<u32>,
    pub read_double_step: bool,

    pub chosen_format: String,
    pub chosen_drive: String,

    pub read_job: Option<ReadJob>,
    /// Background `gw rpm` measurement, and the last reading shown next to the
    /// "Test drive RPM" menu item (kept after the job clears).
    pub rpm_job: Option<RpmJob>,
    pub rpm_result: Option<String>,
    pub read_outcome: Option<Result<String, String>>,

    pub write_state: ListState,
    pub write_erase: bool,
    chosen_source: PathBuf,
    pub chosen_source_name: String,
    pub write_job: Option<WriteJob>,
    pub write_outcome: Option<Result<String, String>>,

    pub driver_items: Vec<FsKind>,
    /// Whether each `driver_items` entry's tool is installed — cached when the
    /// picker opens so rendering doesn't re-probe PATH every frame.
    pub driver_available: Vec<bool>,
    pub driver_index: usize,
    pick_mode: PickMode,
    pub option_items: Vec<CreateOption>,
    /// When editing an fs-format label: (composite key `driver:id`, built-in
    /// fallback label, in-progress text). `None` = not editing.
    pub option_editing: Option<(String, String, TextInput)>,
    pub create_input: TextInput,
    pub browse_image: PathBuf,
    pub browse_driver: FsKind,
    pub browse_format: String,
    /// When browsing a *flux master* (`.hfe`/`.scp`), the path of that master —
    /// `browse_image` is then a decoded working image and every edit is
    /// re-encoded back into this master. `None` for ordinary sector images.
    pub browse_master: Option<PathBuf>,
    /// The `gw` disk format used for the flux-master round-trip convert.
    browse_master_format: String,
    pub create_driver: FsKind,
    pub create_option: String,
    browse_id: i64,
    pub browse_entries: Vec<FileEntry>,
    /// Names of files in the current image that have a stored pristine original.
    pub browse_originals: HashSet<String>,
    pub browse_state: ListState,
    pub browse_usage: Option<FsUsage>,
    pub browse_focus: Focus,
    pub path_input: TextInput,
    pub file_browser: Option<FileBrowser>,
    file_browse_mode: FileBrowseMode,
    host_dir: PathBuf,

    pub clipboard: Vec<ClipItem>,
    pub clip_state: ListState,
    clip_seq: u64,

    pub hex_data: Vec<u8>,
    pub hex_offset: usize,
    pub hex_title: String,
    hex_return: Screen,
    /// Cursor byte index and which nibble (false = high, true = low) while editing.
    pub hex_cursor: usize,
    pub hex_nibble: bool,
    /// Cursor is in the ASCII column (Tab toggles); type printable chars to edit.
    pub hex_ascii: bool,
    /// True once the user has entered overtype/edit mode.
    pub hex_edit: bool,
    /// Unsaved edits pending.
    pub hex_dirty: bool,
    /// Rows the viewer last rendered — used to keep the cursor on-screen.
    pub hex_rows: usize,
    /// The file-in-image being edited (write-back target); `None` = read-only.
    hex_entry: Option<FileEntry>,
    /// The extracted temp file the edited bytes are written back through.
    hex_temp: PathBuf,
    /// Armed after a first Esc with unsaved changes (second Esc discards).
    hex_confirm_discard: bool,

    // --- text editor (format-preserving; parallel to the hex cluster) ---
    /// Editable buffer: logical lines as `char` vectors (cursor col is a char index).
    pub text_lines: Vec<Vec<char>>,
    /// Line ending + trailing-newline state carried from open, re-applied on save.
    pub text_eol: gwm_core::textedit::Eol,
    text_trailing: bool,
    /// Filesystem the file lives in — picks the text codec (v1: always raw/Latin-1).
    text_fs: FsKind,
    pub text_title: String,
    text_return: Screen,
    pub text_row: usize,
    pub text_col: usize,
    /// Top visible logical line (scroll), and rows the editor last rendered.
    pub text_scroll: usize,
    pub text_rows: usize,
    pub text_dirty: bool,
    /// True for a derived, non-editable view (e.g. a detokenised BASIC listing):
    /// navigation only, no edits or save.
    pub text_readonly: bool,
    /// The file-in-image being edited (write-back target).
    text_entry: Option<FileEntry>,
    /// The extracted temp file the edited bytes are written back through.
    text_temp: PathBuf,
    /// Armed after a first Esc with unsaved changes (second Esc discards).
    text_confirm_discard: bool,

    // --- Send to Gotek ---
    /// The image being sent (catalog id, path, display name) and its gw format.
    gotek_id: i64,
    gotek_source: PathBuf,
    pub gotek_name: String,
    gotek_disk_format: Option<String>,
    /// The chosen output format and the format-picker cursor.
    pub gotek_format: GotekFormat,
    pub gotek_format_index: usize,
    /// Detected removable drives and the drive-picker cursor.
    pub gotek_drives: Vec<UsbDrive>,
    pub gotek_drive_index: usize,
    /// Editable output filename for the file written to the Gotek drive.
    pub gotek_name_input: TextInput,
    gotek_job: Option<GotekJob>,
    /// Result of the last send (`Ok(dest)` / `Err(msg)`), shown on the done screen.
    pub gotek_outcome: Option<Result<String, String>>,

    pub tools_index: usize,
    pub tool_status: Vec<bool>,
    /// Per-tool installed version / update state (parallel to `TOOLS`), filled in
    /// off the render thread by [`VersionJob`].
    pub tool_versions: Vec<VersionState>,
    version_job: Option<VersionJob>,
    pub install_job: Option<InstallJob>,
    install_return: Screen,
    /// A shell command to run interactively (suspending the TUI) next frame.
    run_interactive: Option<String>,
    /// Index of the tool whose install command is running, so we can tell whether
    /// it actually landed once the command returns.
    installing_tool: Option<usize>,
    /// A tool install awaiting the user's confirmation: (tool index, command).
    /// Nothing runs until they agree on the ToolConfirm screen.
    pub tool_confirm: Option<(usize, String)>,

    // --- archive.org import ---------------------------------------------
    pub archive_query: TextInput,
    pub archive_hits: Vec<SearchHit>,
    pub archive_hits_state: ListState,
    /// Importable-image count per hit (parallel to `archive_hits`), filled in
    /// by [`CountJob`] in the background so empty items are flagged up front.
    pub archive_counts: Vec<CountState>,
    count_job: Option<CountJob>,
    pub archive_files: Vec<RemoteFile>,
    pub archive_files_state: ListState,
    /// The item whose files are being shown (title, for the header).
    pub archive_item_title: String,
    net_job: Option<NetJob>,
    /// Which screen the finished [`NetJob`] should route to.
    net_target: Screen,
    pub download_job: Option<DownloadJob>,
}

impl App {
    pub fn new(core: Core) -> Self {
        let theme = theme::by_name(&core.settings.theme);
        Self {
            core,
            screen: Screen::Menu,
            theme,
            notice: None,
            should_quit: false,
            flow: Flow::Read,
            settings_index: 0,
            settings_editing: false,
            storage_input: TextInput::new(),
            tune_index: 0,
            tune_values: Vec::new(),
            tuning_name_input: TextInput::new(),
            tuning_profile_index: 0,
            menu_index: 0,
            library: Vec::new(),
            lib_state: ListState::default(),
            lib_filter: String::new(),
            lib_filtering: false,
            lib_subpath: PathBuf::new(),
            rename_input: TextInput::new(),
            move_targets: Vec::new(),
            move_state: ListState::default(),
            move_item: None,
            notes_input: TextInput::new(),
            notes_id: 0,
            folder_input: TextInput::new(),
            delete_file: false,
            verify_results: HashMap::new(),
            formats: Vec::new(),
            format_filter: String::new(),
            format_state: ListState::default(),
            format_editing: None,
            drive_index: 0,
            name_input: TextInput::new(),
            read_hard_sectors: false,
            read_opt_row: 0,
            read_track_start: None,
            read_track_end: None,
            read_double_step: false,
            chosen_format: String::new(),
            chosen_drive: String::new(),
            read_job: None,
            rpm_job: None,
            rpm_result: None,
            read_outcome: None,
            write_state: ListState::default(),
            write_erase: false,
            chosen_source: PathBuf::new(),
            chosen_source_name: String::new(),
            write_job: None,
            write_outcome: None,
            driver_items: Vec::new(),
            driver_available: Vec::new(),
            driver_index: 0,
            pick_mode: PickMode::Browse,
            option_items: Vec::new(),
            option_editing: None,
            create_input: TextInput::new(),
            browse_image: PathBuf::new(),
            browse_driver: FsKind::Cpm,
            browse_format: String::new(),
            browse_master: None,
            browse_master_format: String::new(),
            create_driver: FsKind::Cpm,
            create_option: String::new(),
            browse_id: 0,
            browse_entries: Vec::new(),
            browse_originals: HashSet::new(),
            browse_state: ListState::default(),
            browse_usage: None,
            browse_focus: Focus::Files,
            path_input: TextInput::new(),
            file_browser: None,
            file_browse_mode: FileBrowseMode::InsertFile,
            host_dir: std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/")),
            clipboard: Vec::new(),
            clip_state: ListState::default(),
            clip_seq: 0,
            hex_data: Vec::new(),
            hex_offset: 0,
            hex_title: String::new(),
            hex_return: Screen::Library,
            hex_cursor: 0,
            hex_nibble: false,
            hex_ascii: false,
            hex_edit: false,
            hex_dirty: false,
            hex_rows: 16,
            hex_entry: None,
            hex_temp: PathBuf::new(),
            hex_confirm_discard: false,

            text_lines: Vec::new(),
            text_eol: gwm_core::textedit::Eol::Lf,
            text_trailing: false,
            text_fs: FsKind::Cpm,
            text_title: String::new(),
            text_return: Screen::Browse,
            text_row: 0,
            text_col: 0,
            text_scroll: 0,
            text_rows: 0,
            text_dirty: false,
            text_readonly: false,
            text_entry: None,
            text_temp: PathBuf::new(),
            text_confirm_discard: false,
            gotek_id: 0,
            gotek_source: PathBuf::new(),
            gotek_name: String::new(),
            gotek_disk_format: None,
            gotek_format: GotekFormat::Hfe,
            gotek_format_index: 0,
            gotek_drives: Vec::new(),
            gotek_drive_index: 0,
            gotek_name_input: TextInput::new(),
            gotek_job: None,
            gotek_outcome: None,

            tools_index: 0,
            tool_status: Vec::new(),
            tool_versions: Vec::new(),
            version_job: None,
            install_job: None,
            install_return: Screen::Tools,
            run_interactive: None,
            installing_tool: None,
            tool_confirm: None,
            archive_query: TextInput::new(),
            archive_hits: Vec::new(),
            archive_hits_state: ListState::default(),
            archive_files: Vec::new(),
            archive_files_state: ListState::default(),
            archive_item_title: String::new(),
            archive_counts: Vec::new(),
            count_job: None,
            net_job: None,
            net_target: Screen::ArchiveResults,
            download_job: None,
        }
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        self.reload_library()?;
        let tick = Duration::from_millis(80);
        while !self.should_quit {
            terminal.draw(|frame| crate::ui::render(&mut self, frame))?;

            match self.screen {
                Screen::Reading => {
                    if self.read_job.as_mut().map(ReadJob::pump).unwrap_or(false) {
                        self.finalize_read();
                    }
                }
                Screen::Writing => {
                    if self.write_job.as_mut().map(WriteJob::pump).unwrap_or(false) {
                        self.finalize_write();
                    }
                }
                Screen::Installing => {
                    if self.install_job.as_mut().map(InstallJob::pump).unwrap_or(false) {
                        self.refresh_tool_status();
                    }
                }
                Screen::ArchiveFetching => {
                    if self.net_job.as_mut().map(NetJob::pump).unwrap_or(false) {
                        self.finalize_net_fetch();
                    }
                }
                Screen::ArchiveDownloading => {
                    if self.download_job.as_mut().map(DownloadJob::pump).unwrap_or(false) {
                        self.finalize_download();
                    }
                }
                _ => {}
            }

            // Result image-counts fill in the background regardless of screen,
            // so browsing into an item and back doesn't restart the enrichment.
            if let Some(mut job) = self.count_job.take() {
                job.pump(&mut self.archive_counts);
                if !job.is_done() {
                    self.count_job = Some(job);
                }
            }

            // Tool versions fill in the background too (probing spawns the tools).
            if let Some(mut job) = self.version_job.take() {
                job.pump(&mut self.tool_versions);
                if !job.is_done() {
                    self.version_job = Some(job);
                }
            }

            // A drive-RPM measurement completes in the background so the menu
            // stays responsive; fold its result into the menu note when it lands.
            if let Some(mut job) = self.rpm_job.take() {
                if job.pump() {
                    self.rpm_result = Some(match job.result {
                        Some(Ok(rpm)) => format!("{rpm:.1} RPM"),
                        Some(Err(e)) => format!("failed: {e}"),
                        None => "no reading".to_string(),
                    });
                } else {
                    self.rpm_job = Some(job);
                }
            }

            // A running "Send to Gotek" convert+copy.
            if let Some(mut job) = self.gotek_job.take() {
                if job.pump() {
                    self.gotek_outcome = Some(match &job.outcome {
                        Some(Ok(())) => Ok(job.dest.display().to_string()),
                        Some(Err(e)) => Err(e.clone()),
                        None => Err("no result".to_string()),
                    });
                    self.screen = Screen::GotekDone;
                } else {
                    self.gotek_job = Some(job);
                }
            }

            // An interactive install command suspends the TUI, runs in the real
            // terminal (so the package manager can prompt), then resumes.
            if let Some(cmd) = self.run_interactive.take() {
                self.run_interactive_command(terminal, &cmd);
                continue;
            }

            if event::poll(tick)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.handle_key(key.code, key.modifiers);
                    }
                }
            }
        }
        Ok(())
    }

    fn reload_library(&mut self) -> Result<()> {
        self.library = self.core.catalog.list()?;
        self.lib_state
            .select(if self.library.is_empty() { None } else { Some(0) });
        Ok(())
    }

    /// Leave the TUI, run a command in the real terminal (so it can prompt for
    /// sudo / review a PKGBUILD), wait for the user, then resume.
    fn run_interactive_command(&mut self, terminal: &mut DefaultTerminal, cmd: &str) {
        use std::io::Write;
        ratatui::restore();
        let mut out = std::io::stdout();
        let _ = writeln!(out, "\n\x1b[1;36m== The Lube Shop ==\x1b[0m running:\n  {cmd}\n");
        let _ = out.flush();

        #[cfg(windows)]
        let _ = std::process::Command::new("cmd").arg("/c").arg(cmd).status();
        #[cfg(not(windows))]
        let _ = std::process::Command::new("sh").arg("-c").arg(cmd).status();

        let _ = writeln!(out, "\n\x1b[1m== Finished. Press Enter to return to The Lube Shop. ==\x1b[0m");
        let _ = out.flush();
        let mut buf = String::new();
        let _ = std::io::stdin().read_line(&mut buf);

        *terminal = ratatui::init();
        // An installer may have appended the tool's dir to the persisted PATH
        // (winget does this for VICE) without updating this running process —
        // pull those entries in so the check below actually finds the new tool.
        gwm_core::tools::refresh_path_from_registry();
        // A folder-style bundle (e.g. gw) just created a new subdir under our bin
        // dir; re-scan so it's on this process's PATH before the check below.
        gwm_core::tools::ensure_user_path();
        self.refresh_tool_status();
        self.screen = Screen::Tools;

        // If the command ran but the tool still isn't on PATH, the package likely
        // doesn't exist for this distro (e.g. Debian dropped `vice`). Point the
        // user at the tool's homepage instead of leaving them with a raw error.
        if let Some(i) = self.installing_tool.take() {
            if !self.tool_status.get(i).copied().unwrap_or(false) {
                let tool = gwm_core::tools::TOOLS[i];
                self.notice = Some(format!(
                    "{} still isn't installed — it may not be packaged for your system. Get it from: {}",
                    tool.label, tool.homepage
                ));
            }
        }
    }

    /// Open the Library, first importing any new files dropped into the storage
    /// folder so hand-placed images show up.
    fn enter_library(&mut self) {
        match gwm_core::library::scan_import(&self.core.catalog, &self.core.paths.library_dir) {
            Ok(n) if n > 0 => {
                self.notice = Some(format!("Imported {n} new file(s) from the storage folder."))
            }
            Err(err) => self.notice = Some(format!("Storage-folder scan failed: {err}")),
            _ => {}
        }
        self.lib_filter.clear();
        self.lib_filtering = false;
        let _ = self.reload_library();
        self.screen = Screen::Library;
    }

    /// The effective label for a format: the user's override if set, else the
    /// generated best-guess description.
    pub fn format_label(&self, fmt: &str) -> String {
        self.core
            .settings
            .format_labels
            .get(fmt)
            .cloned()
            .unwrap_or_else(|| formats::describe_format(fmt))
    }

    pub fn filtered_formats(&self) -> Vec<&str> {
        let needle = self.format_filter.to_lowercase();
        let recent = &self.core.settings.recent_formats;
        let mut matches: Vec<&str> = self
            .formats
            .iter()
            .filter(|f| {
                needle.is_empty()
                    || f.to_lowercase().contains(&needle)
                    || self.format_label(f).to_lowercase().contains(&needle)
            })
            .map(String::as_str)
            .collect();
        // Float recently-used formats to the top (stable: others keep gw order).
        matches.sort_by_key(|f| {
            recent
                .iter()
                .position(|r| r == f)
                .map(|i| i as i64)
                .unwrap_or(i64::MAX)
        });
        matches
    }

    pub fn is_recent_format(&self, fmt: &str) -> bool {
        self.core.settings.recent_formats.iter().any(|f| f == fmt)
    }

    fn record_recent_format(&mut self, fmt: &str) {
        let recents = &mut self.core.settings.recent_formats;
        recents.retain(|f| f != fmt);
        recents.insert(0, fmt.to_string());
        recents.truncate(6);
        let _ = self.core.save_settings();
    }

    // --- input routing ---------------------------------------------------

    #[cfg(test)]
    pub fn test_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        self.handle_key(code, mods);
    }

    fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        // A notice is transient: the next keypress dismisses it (a handler below
        // may then set a fresh one, which survives until the following key).
        self.notice = None;
        match self.screen {
            Screen::Menu => self.on_menu_key(code),
            Screen::Library => self.on_library_key(code, mods),
            Screen::LibraryConfirmDelete => self.on_library_delete_key(code),
            Screen::LibraryRename => self.on_library_rename_key(code, mods),
            Screen::LibraryMove => self.on_library_move_key(code),
            Screen::EditNotes => self.on_edit_notes_key(code, mods),
            Screen::NewFolder => self.on_new_folder_key(code, mods),
            Screen::FormatPicker => self.on_format_key(code, mods),
            Screen::DrivePicker => self.on_drive_key(code),
            Screen::NameInput => self.on_name_key(code, mods),
            Screen::ReadOptions => self.on_read_options_key(code),
            Screen::WriteSource => self.on_write_source_key(code),
            Screen::WriteConfirm => self.on_write_confirm_key(code),
            Screen::Reading => self.on_reading_key(code),
            Screen::Writing => {} // destructive — runs to completion; Ctrl+C quits
            Screen::ReadDone | Screen::WriteDone => self.on_done_key(code),
            Screen::Settings => self.on_settings_key(code, mods),
            Screen::DriverPicker => self.on_driver_key(code),
            Screen::OptionPicker => self.on_option_key(code, mods),
            Screen::DriveTuning => self.on_drive_tuning_key(code),
            Screen::TuningSaveName => self.on_tuning_save_key(code, mods),
            Screen::TuningProfiles => self.on_tuning_profiles_key(code),
            Screen::Browse => self.on_browse_key(code),
            Screen::BrowseInput => self.on_browse_input_key(code, mods),
            Screen::BrowseConfirmDelete => self.on_browse_delete_key(code),
            Screen::FileBrowse => self.on_file_browse_key(code, mods),
            Screen::HexView => self.on_hex_key(code, mods),
            Screen::TextEdit => self.on_text_key(code, mods),
            Screen::GotekFormat => self.on_gotek_format_key(code),
            Screen::GotekDrive => self.on_gotek_drive_key(code),
            Screen::GotekName => self.on_gotek_name_key(code, mods),
            Screen::GotekSending => {}
            Screen::GotekDone => {
                if matches!(code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
                    self.screen = Screen::Library;
                }
            }
            Screen::NewImageName => self.on_new_image_key(code, mods),
            Screen::Tools => self.on_tools_key(code),
            Screen::ToolConfirm => self.on_tool_confirm_key(code),
            Screen::Installing => self.on_installing_key(code),
            Screen::ArchiveSearch => self.on_archive_search_key(code, mods),
            Screen::ArchiveFetching => self.on_archive_fetching_key(code),
            Screen::ArchiveResults => self.on_archive_results_key(code),
            Screen::ArchiveFiles => self.on_archive_files_key(code),
            Screen::ArchiveDownloading => {} // runs to completion; Ctrl+C quits
        }
    }

    // --- image browsing (cpmtools) --------------------------------------

    // --- Send to Gotek --------------------------------------------------

    fn start_gotek(&mut self) {
        let Some(it) = self.selected_file() else {
            return;
        };
        self.gotek_id = it.id;
        self.gotek_source = PathBuf::from(&it.path);
        self.gotek_name = item_file_name(&it);
        self.gotek_disk_format = it.format.clone();
        self.gotek_format_index = 0;
        self.gotek_format = GOTEK_FORMATS[0];
        self.screen = Screen::GotekFormat;
    }

    fn on_gotek_format_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Library,
            KeyCode::Up | KeyCode::Char('k') => {
                self.gotek_format_index = self
                    .gotek_format_index
                    .checked_sub(1)
                    .unwrap_or(GOTEK_FORMATS.len() - 1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.gotek_format_index = (self.gotek_format_index + 1) % GOTEK_FORMATS.len();
            }
            KeyCode::Enter => {
                self.gotek_format = GOTEK_FORMATS[self.gotek_format_index];
                self.refresh_gotek_drives();
                self.gotek_drive_index = 0;
                self.screen = Screen::GotekDrive;
            }
            _ => {}
        }
    }

    fn refresh_gotek_drives(&mut self) {
        self.gotek_drives = gwm_core::usb::removable_drives();
        if self.gotek_drive_index >= self.gotek_drives.len() {
            self.gotek_drive_index = self.gotek_drives.len().saturating_sub(1);
        }
    }

    fn on_gotek_drive_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::GotekFormat,
            KeyCode::Char('r') => self.refresh_gotek_drives(),
            KeyCode::Up | KeyCode::Char('k') => {
                move_list_index(&mut self.gotek_drive_index, self.gotek_drives.len(), -1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                move_list_index(&mut self.gotek_drive_index, self.gotek_drives.len(), 1)
            }
            KeyCode::Enter => {
                if !self.gotek_drives.is_empty() {
                    // Let the user name the file before it's written.
                    self.gotek_name_input
                        .set(gotek_out_name(&self.gotek_name, self.gotek_format));
                    self.screen = Screen::GotekName;
                }
            }
            _ => {}
        }
    }

    fn on_gotek_name_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Esc => self.screen = Screen::GotekDrive,
            KeyCode::Enter => {
                let name = self.gotek_name_input.text().trim().replace(['/', '\\'], "_");
                if name.is_empty() {
                    self.notice = Some("Enter a filename.".to_string());
                    return;
                }
                self.start_gotek_send(&self.gotek_final_name(&name));
            }
            _ => edit_input(&mut self.gotek_name_input, code, mods),
        }
    }

    /// Ensure the chosen name carries the right extension for the format (HFE modes
    /// need `.hfe` so the Gotek recognises it; copy-as-is is left exactly as typed).
    fn gotek_final_name(&self, typed: &str) -> String {
        match self.gotek_format.extension() {
            Some(ext) if !typed.to_ascii_lowercase().ends_with(&format!(".{ext}")) => {
                format!("{typed}.{ext}")
            }
            _ => typed.to_string(),
        }
    }

    fn start_gotek_send(&mut self, out_name: &str) {
        let Some(drive) = self.gotek_drives.get(self.gotek_drive_index).cloned() else {
            return;
        };
        let format = self.gotek_format;
        let dest = drive.mount.join(out_name);
        let temp_dir = self.clip_dir();
        let _ = std::fs::create_dir_all(&temp_dir);
        let temp = temp_dir.join(format!("gotek-{}", safe_host_name(out_name)));
        self.gotek_outcome = None;
        self.gotek_job = Some(GotekJob::start(
            self.gotek_source.clone(),
            temp,
            dest,
            format,
            self.gotek_disk_format.clone(),
        ));
        self.screen = Screen::GotekSending;
    }

    fn start_browse(&mut self) {
        let Some(it) = self.selected_file() else {
            return;
        };
        let name = item_file_name(&it);
        let ext = Path::new(&name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        // A flux/bit-stream master can't be read by the filesystem tools; decode
        // it to a sector image first (edits are re-encoded back on save).
        if matches!(it.kind, MediaKind::Flux) || formats::is_flux_suffix(ext) {
            self.begin_flux_browse(&it);
            return;
        }

        let (id, path, fs_driver, fs_format) =
            (it.id, it.path.clone(), it.fs_driver.clone(), it.fs_format.clone());
        self.browse_id = id;
        self.browse_image = PathBuf::from(&path);
        // Ordinary image: not a decoded master.
        self.browse_master = None;
        self.browse_master_format.clear();

        // Known driver → open directly (asking a CP/M format only if needed).
        if let Some(driver) = fs_driver.as_deref().and_then(FsKind::from_id) {
            self.browse_driver = driver;
            if driver.needs_format() {
                match fs_format {
                    Some(fmt) => {
                        self.browse_format = fmt;
                        self.open_browser();
                    }
                    None => self.open_browse_format_picker(),
                }
            } else {
                // Self-describing filesystem: no format applies — drop any stale
                // one so it isn't shown in the header.
                self.browse_format.clear();
                self.open_browser();
            }
            return;
        }

        // Unknown → pick a driver, pre-selecting the extension guess.
        self.open_driver_picker(PickMode::Browse, FsKind::guess_from_ext(ext));
    }

    /// Browse a flux/bit-stream master (`.hfe`/`.scp`): decode it to a working
    /// sector image and open the browser on that. The master stays the source of
    /// truth — edits are folded back in `after_image_modified`.
    fn begin_flux_browse(&mut self, it: &MediaItem) {
        self.browse_id = it.id;
        self.browse_master = Some(PathBuf::from(&it.path));
        self.browse_master_format.clear();
        // Carry over the browse filesystem format the master already remembers
        // (used for CP/M).
        self.browse_format = it.fs_format.clone().unwrap_or_default();

        // Route by the target filesystem. TRS-80 can't be decoded by gw at all
        // (no format, and gw can't write DMK) — it goes through HxC. Everything
        // else decodes with gw. If the driver isn't remembered yet, ask for it —
        // that's the CP/M · FAT · Commodore · TRS-80 · Amiga · Apple menu.
        match it.fs_driver.as_deref().and_then(FsKind::from_id) {
            Some(FsKind::Trs) => self.decode_trs_flux_to_library(),
            Some(driver) => {
                self.browse_driver = driver;
                self.begin_gw_decode();
            }
            None => self.open_driver_picker(PickMode::Browse, None),
        }
    }

    /// Continue a flux browse down the `gw convert` path once the target driver is
    /// known (everything except TRS-80). Needs a gw disk format — the catalogued
    /// one if present, otherwise the user picks it.
    fn begin_gw_decode(&mut self) {
        if !self.gw_ready() {
            self.notice = Some("gw is required to decode this flux capture.".to_string());
            return;
        }
        if self.formats.is_empty() {
            self.formats = formats::list_formats();
        }
        if self.formats.is_empty() {
            self.notice = Some("Could not read the format list from gw.".to_string());
            return;
        }
        // Always ask which format to decode a flux master with: the right choice
        // (single- vs double-sided, hard-sectored, …) can't be derived from the
        // file, and a wrong pick used to be saved with no way to change it. The
        // last-used format is pre-selected, so confirming it is a single keystroke
        // while a different disk type is still one filter away.
        let remembered = self
            .master_item()
            .and_then(|m| m.format)
            .filter(|f| !f.trim().is_empty());
        self.flow = Flow::Decode;
        self.format_filter.clear();
        let idx = {
            let ff = self.filtered_formats();
            remembered
                .as_deref()
                .and_then(|w| ff.iter().position(|f| *f == w))
                .unwrap_or(0)
        };
        self.format_state.select(Some(idx));
        self.notice = Some("Pick the disk format to decode this flux capture.".to_string());
        self.screen = Screen::FormatPicker;
    }

    /// The catalog row for the flux master currently being browsed.
    fn master_item(&self) -> Option<MediaItem> {
        self.library.iter().find(|x| x.id == self.browse_id).cloned()
    }

    /// Where a flux master's decoded working image lives (outside the library so
    /// the importer never picks it up). Re-created from the master on each browse.
    fn flux_work_path(&self, ext: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lubeshop-flux-{}.{ext}", self.browse_id))
    }

    /// Decode the current flux master with `gw_format` and open the browser on the
    /// resulting sector image. The target driver is already set in
    /// `self.browse_driver`.
    fn decode_and_open(&mut self, gw_format: &str) {
        let Some(master) = self.browse_master.clone() else {
            return;
        };
        self.browse_master_format = gw_format.to_string();
        // Remember the format on the master so we skip the picker next time.
        let _ = self.core.catalog.update_format(self.browse_id, gw_format);

        let ext = formats::decoded_container_ext(gw_format);
        let work = self.flux_work_path(ext);
        if let Err(err) = convert::convert(&master, &work, gw_format) {
            self.notice = Some(format!("Could not decode {}: {err}", master.display()));
            self.screen = Screen::Library;
            return;
        }
        self.browse_image = work;
        // CP/M still needs a diskdef; other drivers self-describe.
        if self.browse_driver.needs_format() && self.browse_format.is_empty() {
            self.open_browse_format_picker();
        } else {
            self.open_browser();
        }
    }

    /// Decode a TRS-80 flux capture (`.raw`/`.hfe`) to a real `.dmk` saved beside
    /// the master in the library, catalogue it as a derived image, and browse it
    /// as an ordinary DMK. One-way: flux can't be regenerated from edited sectors,
    /// so the DMK — not the flux — becomes the thing you keep and edit.
    fn decode_trs_flux_to_library(&mut self) {
        if !convert::hxcfe_available() {
            self.notice = Some(
                "HxC (hxcfe) is required to decode TRS-80 flux — install it from Tools."
                    .to_string(),
            );
            return;
        }
        let Some(master) = self.browse_master.clone() else {
            return;
        };
        let dmk = unique_sibling(&master, "dmk");
        if let Err(err) = convert::flux_to_dmk(&master, &dmk) {
            self.notice = Some(format!("Could not decode {}: {err}", master.display()));
            self.screen = Screen::Library;
            return;
        }
        let size = std::fs::metadata(&dmk).map(|m| m.len() as i64).unwrap_or(0);
        let sha = gwm_core::util::sha256_file(&dmk).ok();
        let from = master
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let item = NewMediaItem {
            kind: MediaKind::Image,
            path: dmk.to_string_lossy().into_owned(),
            format: None,
            system: Some("TRS-80".to_string()),
            size_bytes: size,
            sha256: sha,
            source: Source::Import,
            remote_id: None,
            tags: Vec::new(),
            notes: Some(format!("Decoded from flux {from}")),
            fs_format: None,
            fs_driver: Some(FsKind::Trs.id().to_string()),
        };
        match self.core.catalog.insert(&item) {
            Ok(id) => {
                let _ = self.reload_library();
                // Browse the new DMK as an ordinary library image, not a master.
                self.browse_id = id;
                self.browse_image = dmk;
                self.browse_master = None;
                self.browse_master_format.clear();
                self.browse_driver = FsKind::Trs;
                self.browse_format.clear();
                self.notice = Some(format!("Decoded flux to {}", item.path));
                self.open_browser();
            }
            Err(err) => self.notice = Some(format!("Decoded, but cataloguing failed: {err}")),
        }
    }

    /// Re-pick the filesystem driver/format for the selected library image
    /// without having to open the (possibly garbled) browser first. Opens the
    /// driver picker pre-set to the remembered driver, or the extension guess.
    fn reformat_selected(&mut self) {
        let Some(it) = self.selected_file() else { return };
        self.browse_id = it.id;
        self.browse_image = PathBuf::from(&it.path);
        let name = item_file_name(&it);
        let current = it.fs_driver.as_deref().and_then(FsKind::from_id);
        let preselect = current.or_else(|| {
            let ext = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("");
            FsKind::guess_from_ext(ext)
        });
        self.open_driver_picker(PickMode::Browse, preselect);
    }

    /// A driver instance for the currently-browsed image.
    fn browse_fs(&self) -> Box<dyn ImageFs> {
        let format = self
            .browse_driver
            .needs_format()
            .then_some(self.browse_format.as_str());
        self.browse_driver.open(format)
    }

    fn open_driver_picker(&mut self, mode: PickMode, preselect: Option<FsKind>) {
        // Show every applicable driver — including ones whose tool isn't installed
        // yet — so a user with, say, a FAT image always sees "FAT" and can be sent
        // to install mtools, instead of the option silently vanishing. Availability
        // is cached here (it probes PATH) so rendering stays cheap.
        self.driver_items = FsKind::ALL
            .iter()
            .copied()
            // Read-only drivers (TRS-80) can't create blank images.
            .filter(|k| !matches!(mode, PickMode::Create) || k.can_create())
            .collect();
        self.driver_available = self.driver_items.iter().map(|k| k.available()).collect();
        self.pick_mode = mode;
        self.driver_index = preselect
            .and_then(|p| self.driver_items.iter().position(|k| *k == p))
            .unwrap_or(0);
        self.screen = Screen::DriverPicker;
    }

    fn on_driver_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.screen = match self.pick_mode {
                    PickMode::Browse => Screen::Library,
                    PickMode::Create => Screen::Menu,
                };
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let n = self.driver_items.len();
                if n > 0 {
                    self.driver_index = self.driver_index.checked_sub(1).unwrap_or(n - 1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let n = self.driver_items.len();
                if n > 0 {
                    self.driver_index = (self.driver_index + 1) % n;
                }
            }
            KeyCode::Enter => {
                let Some(driver) = self.driver_items.get(self.driver_index).copied() else {
                    return;
                };
                // Tool not installed: send the user to the Tools menu (with that
                // tool preselected) rather than proceeding to a guaranteed failure.
                if !self.driver_available.get(self.driver_index).copied().unwrap_or(true) {
                    self.notice = Some(format!(
                        "{} needs its tool installed — press Enter in Tools to get it.",
                        driver.short_label()
                    ));
                    self.enter_tools();
                    if let Some(i) = tool_index_for_driver(driver) {
                        self.tools_index = i;
                    }
                    return;
                }
                match self.pick_mode {
                    PickMode::Browse => {
                        self.browse_driver = driver;
                        let _ = self.core.catalog.update_fs_driver(self.browse_id, driver.id());
                        let _ = self.reload_library();
                        if self.browse_master.is_some() {
                            // Decoding a flux master: route to the right engine.
                            if driver == FsKind::Trs {
                                self.decode_trs_flux_to_library();
                            } else {
                                self.begin_gw_decode();
                            }
                        } else if driver.needs_format() {
                            self.open_browse_format_picker();
                        } else {
                            // Self-describing filesystem: clear any stale format.
                            self.browse_format.clear();
                            self.open_browser();
                        }
                    }
                    PickMode::Create => {
                        self.create_driver = driver;
                        self.open_option_picker(driver.create_options());
                    }
                }
            }
            _ => {}
        }
    }

    fn open_browse_format_picker(&mut self) {
        let options = self
            .browse_driver
            .browse_formats()
            .into_iter()
            .map(|f| CreateOption { label: gwm_core::imagefs::describe_diskdef(&f), id: f })
            .collect();
        self.pick_mode = PickMode::Browse;
        self.open_option_picker(options);
    }

    fn open_option_picker(&mut self, options: Vec<CreateOption>) {
        self.option_items = options;
        self.format_filter.clear();
        self.format_state
            .select((!self.option_items.is_empty()).then_some(0));
        self.screen = Screen::OptionPicker;
    }

    /// The driver whose formats the option picker is currently showing.
    fn option_driver(&self) -> FsKind {
        match self.pick_mode {
            PickMode::Browse => self.browse_driver,
            PickMode::Create => self.create_driver,
        }
    }

    /// Settings key for an fs-format override (`driver:id`).
    fn fs_format_key(&self, id: &str) -> String {
        format!("{}:{}", self.option_driver().id(), id)
    }

    /// The effective label for an fs-format option: user override, else the
    /// option's built-in/generated label.
    pub fn fs_option_label(&self, opt: &CreateOption) -> String {
        self.core
            .settings
            .fs_format_labels
            .get(&self.fs_format_key(&opt.id))
            .cloned()
            .unwrap_or_else(|| opt.label.clone())
    }

    pub fn fs_option_is_custom(&self, id: &str) -> bool {
        self.core.settings.fs_format_labels.contains_key(&self.fs_format_key(id))
    }

    pub fn filtered_options(&self) -> Vec<&CreateOption> {
        let needle = self.format_filter.to_lowercase();
        let recent = &self.core.settings.recent_fs_formats;
        let mut matches: Vec<&CreateOption> = self
            .option_items
            .iter()
            .filter(|o| {
                needle.is_empty()
                    || o.id.to_lowercase().contains(&needle)
                    || self.fs_option_label(o).to_lowercase().contains(&needle)
            })
            .collect();
        // Float recently-used formats to the top (stable otherwise).
        matches.sort_by_key(|o| {
            recent
                .iter()
                .position(|r| r == &o.id)
                .map(|i| i as i64)
                .unwrap_or(i64::MAX)
        });
        matches
    }

    pub fn is_recent_fs_format(&self, id: &str) -> bool {
        self.core.settings.recent_fs_formats.iter().any(|f| f == id)
    }

    fn record_recent_fs_format(&mut self, id: &str) {
        let recents = &mut self.core.settings.recent_fs_formats;
        recents.retain(|f| f != id);
        recents.insert(0, id.to_string());
        recents.truncate(6);
        let _ = self.core.save_settings();
    }

    fn reset_option_selection(&mut self) {
        let any = !self.filtered_options().is_empty();
        self.format_state.select(any.then_some(0));
    }

    /// Open the label editor for the selected fs-format option, pre-filled with
    /// its effective label.
    fn begin_option_label_edit(&mut self) {
        let Some(opt) = self
            .format_state
            .selected()
            .and_then(|i| self.filtered_options().get(i).map(|o| (*o).clone()))
        else {
            return;
        };
        let key = self.fs_format_key(&opt.id);
        let mut input = TextInput::new();
        input.set(self.fs_option_label(&opt));
        self.option_editing = Some((key, opt.label, input));
    }

    fn on_option_label_edit_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Esc => self.option_editing = None,
            KeyCode::Enter => {
                if let Some((key, builtin, input)) = self.option_editing.take() {
                    let text = input.text().trim().to_string();
                    let labels = &mut self.core.settings.fs_format_labels;
                    if text.is_empty() || text == builtin {
                        labels.remove(&key);
                        self.notice = Some(format!("Reset {key} to its default label."));
                    } else {
                        labels.insert(key.clone(), text);
                        self.notice = Some(format!("Saved label for {key}."));
                    }
                    let _ = self.core.save_settings();
                }
            }
            _ => {
                if let Some((_, _, input)) = self.option_editing.as_mut() {
                    edit_input(input, code, mods);
                }
            }
        }
    }

    fn on_option_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if self.option_editing.is_some() {
            self.on_option_label_edit_key(code, mods);
            return;
        }
        match code {
            KeyCode::Esc => self.screen = Screen::DriverPicker,
            // Ctrl+E: edit the selected format's label.
            KeyCode::Char('e') if mods.contains(KeyModifiers::CONTROL) => {
                self.begin_option_label_edit();
            }
            KeyCode::Up => {
                let n = self.filtered_options().len();
                move_list(&mut self.format_state, n, -1);
            }
            KeyCode::Down => {
                let n = self.filtered_options().len();
                move_list(&mut self.format_state, n, 1);
            }
            KeyCode::Backspace => {
                self.format_filter.pop();
                self.reset_option_selection();
            }
            KeyCode::Enter => {
                let choice = self
                    .format_state
                    .selected()
                    .and_then(|i| self.filtered_options().get(i).map(|o| (*o).clone()));
                if let Some(opt) = choice {
                    self.record_recent_fs_format(&opt.id);
                    match self.pick_mode {
                        PickMode::Browse => {
                            self.browse_format = opt.id.clone();
                            let _ = self.core.catalog.update_fs_format(self.browse_id, &opt.id);
                            let _ = self.reload_library();
                            self.open_browser();
                        }
                        PickMode::Create => {
                            self.create_option = opt.id.clone();
                            let ext = self.create_driver.default_extension(&opt.id);
                            let base = opt.id.replace(['/', ' '], "_");
                            self.create_input.set(format!("{base}.{ext}"));
                            self.screen = Screen::NewImageName;
                        }
                    }
                }
            }
            KeyCode::Char(c) if is_typing(mods) => {
                self.format_filter.push(c);
                self.reset_option_selection();
            }
            _ => {}
        }
    }

    fn enter_create_flow(&mut self) {
        self.open_driver_picker(PickMode::Create, None);
    }

    fn on_new_image_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Esc => self.screen = Screen::OptionPicker,
            KeyCode::Enter => self.do_create(),
            _ => edit_input(&mut self.create_input, code, mods),
        }
    }

    fn do_create(&mut self) {
        let name = self.create_input.text().trim().replace('/', "_");
        let name = if name.is_empty() {
            format!("new.{}", self.create_driver.default_extension(&self.create_option))
        } else {
            name
        };
        let path = self.lib_base().join(&name);
        if path.exists() {
            self.notice = Some("A file with that name already exists.".to_string());
            return;
        }

        match self.create_driver.create(&self.create_option, &path) {
            Ok(()) => {
                let size = std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);
                let needs_format = self.create_driver.needs_format();
                // Record the physical gw disk format for CP/M diskdefs that map to
                // one (hard-sectored NorthStar/Micropolis), so Send-to-Gotek can
                // encode the disk through gw rather than failing hxcfe.
                let gw_format = needs_format
                    .then(|| formats::gw_format_for_cpm_diskdef(&self.create_option))
                    .flatten()
                    .map(str::to_string);
                let item = NewMediaItem {
                    kind: MediaKind::Image,
                    path: path.to_string_lossy().into_owned(),
                    format: gw_format,
                    system: Some(self.create_driver.label().to_string()),
                    size_bytes: size,
                    sha256: gwm_core::util::sha256_file(&path).ok(),
                    source: Source::Import,
                    remote_id: None,
                    tags: Vec::new(),
                    notes: Some(format!("created · {}", self.create_option)),
                    // Remember the driver (and format, if it needs one) for browsing.
                    fs_format: needs_format.then(|| self.create_option.clone()),
                    fs_driver: Some(self.create_driver.id().to_string()),
                };
                let _ = self.core.catalog.insert(&item);
                let _ = self.reload_library();
                self.notice = Some(format!("Created {name}"));
                self.screen = Screen::Library;
            }
            Err(err) => self.notice = Some(format!("Create failed: {err}")),
        }
    }

    fn open_browser(&mut self) {
        let fs = self.browse_fs();
        match fs.list(&self.browse_image) {
            Ok(entries) => {
                self.browse_entries = entries;
                self.browse_state
                    .select((!self.browse_entries.is_empty()).then_some(0));
                self.browse_usage = fs.usage(&self.browse_image).ok();
                self.write_browse_listing();
                self.load_originals();
                self.browse_focus = Focus::Files;
                if self.clip_state.selected().is_none() && !self.clipboard.is_empty() {
                    self.clip_state.select(Some(0));
                }
                self.screen = Screen::Browse;
            }
            Err(err) => {
                // Wrong driver/format? Let the user re-pick instead of bailing.
                self.open_driver_picker(PickMode::Browse, Some(self.browse_driver));
                self.notice = Some(format!(
                    "Could not read as {}: {err} — or try a different filesystem",
                    self.browse_driver.label()
                ));
            }
        }
    }

    /// Write a `<image>.txt` sidecar listing the disk's contents next to the
    /// real library file (the flux master for a decoded master, else the image
    /// itself — never the temporary decode). Best-effort and silent; refreshed
    /// on every browse, so it also picks up inserts/deletes.
    fn write_browse_listing(&self) {
        let target = self.browse_master.as_ref().unwrap_or(&self.browse_image);
        let desc = if self.browse_format.is_empty() {
            self.browse_driver.label().to_string()
        } else {
            format!("{} — {}", self.browse_driver.label(), self.browse_format)
        };
        let _ = gwm_core::imagefs::write_file_listing(target, &desc, &self.browse_entries);
    }

    fn selected_entry(&self) -> Option<FileEntry> {
        self.browse_state
            .selected()
            .and_then(|i| self.browse_entries.get(i).cloned())
    }

    fn on_browse_key(&mut self, code: KeyCode) {
        // Keys common to both panes.
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => {
                self.screen = Screen::Library;
                // Leaving a decoded flux master: forget it so the next browse of
                // an ordinary image isn't mistaken for one.
                self.browse_master = None;
                self.browse_master_format.clear();
                return;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.browse_focus = match self.browse_focus {
                    Focus::Files => Focus::Clip,
                    Focus::Clip => Focus::Files,
                };
                return;
            }
            _ => {}
        }

        match self.browse_focus {
            Focus::Files => match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    move_list(&mut self.browse_state, self.browse_entries.len(), -1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    move_list(&mut self.browse_state, self.browse_entries.len(), 1)
                }
                KeyCode::Char('r') => self.open_browser(),
                KeyCode::Char('f') => {
                    // Re-pick the filesystem/format for this image.
                    self.open_driver_picker(PickMode::Browse, Some(self.browse_driver));
                }
                KeyCode::Char('c') => self.copy_to_clipboard(),
                KeyCode::Char('x') => {
                    if self.selected_entry().is_some() {
                        self.path_input
                            .set(self.core.paths.library_dir.to_string_lossy().into_owned());
                        self.screen = Screen::BrowseInput;
                    }
                }
                KeyCode::Char('i') => {
                    self.file_browse_mode = FileBrowseMode::InsertFile;
                    self.file_browser = Some(FileBrowser::new(self.host_dir.clone()));
                    self.screen = Screen::FileBrowse;
                }
                KeyCode::Char('d') => {
                    if self.selected_entry().is_some() {
                        self.screen = Screen::BrowseConfirmDelete;
                    }
                }
                KeyCode::Char('R') => self.restore_original(),
                KeyCode::Char('h') => {
                    if let Some(entry) = self.selected_entry() {
                        let dir = self.clip_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        let temp = dir.join(format!("hex-{}", safe_host_name(&entry.name)));
                        match self.browse_fs().extract(&self.browse_image, &entry, &temp) {
                            Ok(()) => {
                                self.open_hex(&temp, entry.name.clone(), Screen::Browse, Some(entry.clone()))
                            }
                            Err(err) => self.notice = Some(format!("Could not read file: {err}")),
                        }
                    }
                }
                KeyCode::Char('t') => {
                    if let Some(entry) = self.selected_entry() {
                        let dir = self.clip_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        let temp = dir.join(format!("txt-{}", safe_host_name(&entry.name)));
                        match self.browse_fs().extract(&self.browse_image, &entry, &temp) {
                            Ok(()) => self.open_text(&temp, entry.name.clone(), entry.clone()),
                            Err(err) => self.notice = Some(format!("Could not read file: {err}")),
                        }
                    }
                }
                _ => {}
            },
            Focus::Clip => match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    move_list(&mut self.clip_state, self.clipboard.len(), -1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    move_list(&mut self.clip_state, self.clipboard.len(), 1)
                }
                KeyCode::Enter | KeyCode::Char('p') => self.paste_from_clipboard(),
                KeyCode::Char('d') => self.remove_from_clipboard(),
                _ => {}
            },
        }
    }

    fn clip_dir(&self) -> PathBuf {
        std::env::temp_dir().join(format!("gwm-clip-{}", std::process::id()))
    }

    fn copy_to_clipboard(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let dir = self.clip_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            self.notice = Some("Could not create clipboard staging folder.".to_string());
            return;
        }
        let staged = dir.join(format!("{}-{}", self.clip_seq, safe_host_name(&entry.name)));
        self.clip_seq += 1;
        match self.browse_fs().extract(&self.browse_image, &entry, &staged) {
            Ok(()) => {
                self.clipboard.push(ClipItem {
                    name: entry.name.clone(),
                    path: staged,
                });
                if self.clip_state.selected().is_none() {
                    self.clip_state.select(Some(0));
                }
                self.notice = Some(format!("Copied {} to the clipboard", entry.name));
            }
            Err(err) => self.notice = Some(format!("Copy failed: {err}")),
        }
    }

    fn paste_from_clipboard(&mut self) {
        let item = self
            .clip_state
            .selected()
            .and_then(|i| self.clipboard.get(i).cloned());
        if let Some(item) = item {
            match self
                .browse_fs()
                .insert(&self.browse_image, &item.path, &item.name, 0)
            {
                Ok(()) => {
                    self.notice = Some(format!("Inserted {} from the clipboard", item.name));
                    self.after_image_modified();
                }
                Err(err) => self.notice = Some(format!("Insert failed: {err}")),
            }
            self.open_browser();
            self.browse_focus = Focus::Clip;
        }
    }

    fn remove_from_clipboard(&mut self) {
        if let Some(i) = self.clip_state.selected() {
            if i < self.clipboard.len() {
                let item = self.clipboard.remove(i);
                let _ = std::fs::remove_file(&item.path);
                let len = self.clipboard.len();
                self.clip_state
                    .select((len > 0).then(|| i.min(len - 1)));
            }
        }
    }

    fn on_browse_input_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Esc => self.screen = Screen::Browse,
            KeyCode::Enter => self.do_extract(),
            _ => edit_input(&mut self.path_input, code, mods),
        }
    }

    fn do_extract(&mut self) {
        let Some(entry) = self.selected_entry() else {
            self.screen = Screen::Browse;
            return;
        };
        let raw = self.path_input.text().trim();
        let mut dest = if raw.is_empty() {
            self.core.paths.library_dir.clone()
        } else {
            PathBuf::from(raw)
        };
        if dest.is_dir() {
            dest = dest.join(safe_host_name(&entry.name));
        }
        match self.browse_fs().extract(&self.browse_image, &entry, &dest) {
            Ok(()) => self.notice = Some(format!("Extracted to {}", dest.display())),
            Err(err) => self.notice = Some(format!("Extract failed: {err}")),
        }
        self.screen = Screen::Browse;
    }

    /// True when the host file browser is choosing a folder (storage-dir setting).
    pub fn file_browse_is_dir_mode(&self) -> bool {
        self.file_browse_mode == FileBrowseMode::PickDir
    }

    fn on_file_browse_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let pick_dir = self.file_browse_mode == FileBrowseMode::PickDir;
        match code {
            KeyCode::Esc => {
                if let Some(fb) = self.file_browser.take() {
                    self.host_dir = fb.dir;
                }
                self.screen = if pick_dir {
                    Screen::Settings
                } else {
                    Screen::Browse
                };
            }
            KeyCode::Up => {
                if let Some(fb) = self.file_browser.as_mut() {
                    fb.move_sel(-1);
                }
            }
            KeyCode::Down => {
                if let Some(fb) = self.file_browser.as_mut() {
                    fb.move_sel(1);
                }
            }
            KeyCode::Left => {
                if let Some(fb) = self.file_browser.as_mut() {
                    fb.go_up();
                }
            }
            // In dir-pick mode: Right descends into the highlighted folder,
            // Enter chooses the *current* folder as the storage dir.
            KeyCode::Right if pick_dir => {
                let _ = self.file_browser.as_mut().and_then(FileBrowser::enter);
            }
            KeyCode::Enter if pick_dir => {
                if let Some(fb) = self.file_browser.take() {
                    let dir = fb.dir.to_string_lossy().into_owned();
                    self.host_dir = fb.dir;
                    match self.core.apply_storage_dir(Some(dir)) {
                        Ok(()) => {
                            let _ = self.reload_library();
                            self.theme = theme::by_name(&self.core.settings.theme);
                            self.notice =
                                Some("Store directory updated — catalog reloaded.".to_string());
                        }
                        Err(err) => {
                            self.notice = Some(format!("Could not set store dir: {err}"))
                        }
                    }
                    self.settings_index = 1;
                    self.screen = Screen::Settings;
                }
            }
            KeyCode::Right | KeyCode::Enter => {
                let picked = self.file_browser.as_mut().and_then(FileBrowser::enter);
                if let Some(file) = picked {
                    if let Some(fb) = self.file_browser.take() {
                        self.host_dir = fb.dir;
                    }
                    self.do_insert_path(&file);
                }
            }
            KeyCode::Backspace => {
                if let Some(fb) = self.file_browser.as_mut() {
                    fb.pop_filter();
                }
            }
            KeyCode::Char(c) if is_typing(mods) => {
                if let Some(fb) = self.file_browser.as_mut() {
                    fb.push_filter(c);
                }
            }
            _ => {}
        }
    }

    fn do_insert_path(&mut self, src: &Path) {
        let name = src
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("FILE")
            .to_string();
        match self.browse_fs().insert(&self.browse_image, src, &name, 0) {
            Ok(()) => {
                self.notice = Some(format!("Inserted {name}"));
                self.after_image_modified();
            }
            Err(err) => self.notice = Some(format!("Insert failed: {err}")),
        }
        self.open_browser();
    }

    fn on_browse_delete_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => self.screen = Screen::Browse,
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(entry) = self.selected_entry() {
                    match self.browse_fs().delete(&self.browse_image, &entry) {
                        Ok(()) => {
                            self.notice = Some(format!("Deleted {}", entry.name));
                            self.after_image_modified();
                        }
                        Err(err) => self.notice = Some(format!("Delete failed: {err}")),
                    }
                }
                self.open_browser();
            }
            _ => {}
        }
    }

    fn after_image_modified(&mut self) {
        // Browsing a flux master? Fold the edit (made to the decoded working
        // image) back into the master, which is the real library artifact whose
        // catalog metadata we then refresh.
        let meta_path = if let Some(master) = self.browse_master.clone() {
            if let Err(err) = convert::convert(
                &self.browse_image,
                &master,
                &self.browse_master_format,
            ) {
                self.notice = Some(format!(
                    "Saved into the image, but re-encoding to {} failed: {err}",
                    master.display()
                ));
            }
            master
        } else {
            self.browse_image.clone()
        };
        let size = std::fs::metadata(&meta_path)
            .map(|m| m.len() as i64)
            .unwrap_or(0);
        let sha = gwm_core::util::sha256_file(&meta_path).ok();
        let _ = self
            .core
            .catalog
            .update_file_meta(self.browse_id, size, sha.as_deref());
        let _ = self.reload_library();
    }

    // --- settings --------------------------------------------------------

    /// Index of the configured default drive within `DRIVE_OPTIONS`.
    fn default_drive_index(&self) -> usize {
        DRIVE_OPTIONS
            .iter()
            .position(|(id, _)| *id == self.core.settings.default_drive)
            .unwrap_or(0)
    }

    fn on_settings_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if self.settings_editing {
            match code {
                KeyCode::Enter => {
                    self.apply_storage_dir();
                    self.settings_editing = false;
                }
                KeyCode::Esc => self.settings_editing = false,
                _ => edit_input(&mut self.storage_input, code, mods),
            }
            return;
        }

        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Menu,
            KeyCode::Up | KeyCode::Char('k') => {
                self.settings_index = self.settings_index.checked_sub(1).unwrap_or(SETTINGS_ROWS - 1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.settings_index = (self.settings_index + 1) % SETTINGS_ROWS;
            }
            KeyCode::Left => self.settings_adjust(-1),
            KeyCode::Right => self.settings_adjust(1),
            KeyCode::Enter => match self.settings_index {
                0 => self.cycle_theme(1),
                1 => {
                    // Browse for the store folder rather than typing a path.
                    let start = {
                        let p = &self.core.paths.store_dir;
                        if p.is_dir() { p.clone() } else { self.host_dir.clone() }
                    };
                    self.file_browse_mode = FileBrowseMode::PickDir;
                    self.file_browser = Some(FileBrowser::new(start));
                    self.screen = Screen::FileBrowse;
                }
                2 => self.cycle_default_drive(1),
                3 => self.enter_drive_tuning(),
                _ => {}
            },
            _ => {}
        }
    }

    fn settings_adjust(&mut self, delta: isize) {
        match self.settings_index {
            0 => self.cycle_theme(delta),
            2 => self.cycle_default_drive(delta),
            _ => {}
        }
    }

    fn cycle_theme(&mut self, delta: isize) {
        let next = theme::cycle(&self.core.settings.theme, delta);
        self.theme = next;
        self.core.settings.theme = next.name.to_string();
        let _ = self.core.save_settings();
    }

    fn cycle_default_drive(&mut self, delta: isize) {
        let current = self.default_drive_index() as isize;
        let len = DRIVE_OPTIONS.len() as isize;
        let next = (current + delta).rem_euclid(len) as usize;
        self.core.settings.default_drive = DRIVE_OPTIONS[next].0.to_string();
        let _ = self.core.save_settings();
    }

    fn apply_storage_dir(&mut self) {
        let dir = self.storage_input.text().trim().to_string();
        let value = if dir.is_empty() { None } else { Some(dir) };
        match self.core.apply_storage_dir(value) {
            Ok(()) => {
                let _ = self.reload_library();
                self.theme = theme::by_name(&self.core.settings.theme);
                self.notice = Some("Store directory updated — catalog reloaded.".to_string());
            }
            Err(err) => self.notice = Some(format!("Could not set store dir: {err}")),
        }
    }

    // --- drive tuning (gw delays) ---------------------------------------

    fn enter_drive_tuning(&mut self) {
        // Seed the working values from the device, overlaid with saved overrides.
        let device = gwm_core::device::get_delays();
        self.tune_values = TUNE_PARAMS
            .iter()
            .map(|p| {
                self.core
                    .settings
                    .tuning
                    .get(p.name)
                    .copied()
                    .or_else(|| device.get(p.name).copied())
                    .unwrap_or(0)
            })
            .collect();
        self.tune_index = 0;
        self.screen = Screen::DriveTuning;
    }

    fn on_drive_tuning_key(&mut self, code: KeyCode) {
        let count = TUNE_PARAMS.len();
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.settings_index = 3;
                self.screen = Screen::Settings;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.tune_index = self.tune_index.checked_sub(1).unwrap_or(count - 1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.tune_index = (self.tune_index + 1) % count;
            }
            KeyCode::Left => self.adjust_tune(-1),
            KeyCode::Right => self.adjust_tune(1),
            KeyCode::Char('r') => self.reset_tuning(),
            KeyCode::Char('s') => self.enter_tuning_save(),
            KeyCode::Char('l') => self.enter_tuning_profiles(),
            _ => {}
        }
    }

    /// Prompt for a name to save the current timings under as a profile.
    fn enter_tuning_save(&mut self) {
        self.tuning_name_input = TextInput::new();
        self.screen = Screen::TuningSaveName;
    }

    fn on_tuning_save_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Esc => self.screen = Screen::DriveTuning,
            KeyCode::Enter => {
                let name = self.tuning_name_input.text().trim().to_string();
                if name.is_empty() {
                    self.notice = Some("Enter a name for the profile.".to_string());
                    return;
                }
                // Snapshot every parameter's current value (not just overrides),
                // so recalling the profile reproduces the whole timing set.
                let profile: std::collections::HashMap<String, u32> = TUNE_PARAMS
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (p.name.to_string(), self.tune_values.get(i).copied().unwrap_or(p.default)))
                    .collect();
                let existed = self.core.settings.tuning_profiles.contains_key(&name);
                self.core.settings.tuning_profiles.insert(name.clone(), profile);
                let _ = self.core.save_settings();
                self.notice = Some(format!(
                    "{} timing profile \"{name}\".",
                    if existed { "Updated" } else { "Saved" }
                ));
                self.screen = Screen::DriveTuning;
            }
            _ => edit_input(&mut self.tuning_name_input, code, mods),
        }
    }

    /// Open the recall picker: "Default (factory)" plus every saved profile.
    fn enter_tuning_profiles(&mut self) {
        self.tuning_profile_index = 0;
        self.screen = Screen::TuningProfiles;
    }

    fn on_tuning_profiles_key(&mut self, code: KeyCode) {
        let names: Vec<String> = self.core.settings.tuning_profiles.keys().cloned().collect();
        let count = names.len() + 1; // row 0 is the built-in "Default (factory)"
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::DriveTuning,
            KeyCode::Up | KeyCode::Char('k') => {
                self.tuning_profile_index = self.tuning_profile_index.checked_sub(1).unwrap_or(count - 1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.tuning_profile_index = (self.tuning_profile_index + 1) % count;
            }
            KeyCode::Enter => {
                if self.tuning_profile_index == 0 {
                    self.reset_tuning(); // Default → factory gw delays
                } else if let Some(name) = names.get(self.tuning_profile_index - 1) {
                    self.load_tuning_profile(&name.clone());
                }
                self.screen = Screen::DriveTuning;
            }
            KeyCode::Char('d') | KeyCode::Char('x') => {
                // Delete the selected saved profile (row 0, Default, can't be deleted).
                if self.tuning_profile_index >= 1 {
                    if let Some(name) = names.get(self.tuning_profile_index - 1).cloned() {
                        self.core.settings.tuning_profiles.remove(&name);
                        let _ = self.core.save_settings();
                        self.notice = Some(format!("Deleted profile \"{name}\"."));
                        let new_count = names.len(); // one fewer profile, plus Default
                        self.tuning_profile_index = self.tuning_profile_index.min(new_count - 1);
                    }
                }
            }
            _ => {}
        }
    }

    /// Load a saved profile into the live `tuning`, apply it, and refresh the row values.
    fn load_tuning_profile(&mut self, name: &str) {
        let Some(profile) = self.core.settings.tuning_profiles.get(name).cloned() else {
            return;
        };
        self.core.settings.tuning = profile.clone();
        let _ = self.core.save_settings();
        let _ = gwm_core::device::apply_delays(&profile);
        self.tune_values = TUNE_PARAMS
            .iter()
            .map(|p| profile.get(p.name).copied().unwrap_or(p.default))
            .collect();
        self.notice = Some(format!("Loaded timing profile \"{name}\"."));
    }

    fn reset_tuning(&mut self) {
        self.core.settings.tuning.clear();
        let _ = self.core.save_settings();
        // Explicitly push gw's factory defaults to the device (a `gw reset`
        // does NOT restore delays).
        let defaults: std::collections::HashMap<String, u32> = TUNE_PARAMS
            .iter()
            .map(|p| (p.name.to_string(), p.default))
            .collect();
        let _ = gwm_core::device::apply_delays(&defaults);
        self.tune_values = TUNE_PARAMS.iter().map(|p| p.default).collect();
        self.notice = Some("Reset all drive timings to gw defaults.".to_string());
    }

    fn adjust_tune(&mut self, dir: isize) {
        let param = &TUNE_PARAMS[self.tune_index];
        let current = self.tune_values.get(self.tune_index).copied().unwrap_or(0);
        let next = if dir < 0 {
            current.saturating_sub(param.step)
        } else {
            (current + param.step).min(param.max)
        };
        if let Some(v) = self.tune_values.get_mut(self.tune_index) {
            *v = next;
        }
        self.core.settings.tuning.insert(param.name.to_string(), next);
        let _ = self.core.save_settings();
        let _ = gwm_core::device::apply_delays(&self.core.settings.tuning);
        self.notice = Some(format!("{} set to {} {}", param.label, next, param.unit));
    }

    // --- archive.org import ---------------------------------------------

    /// The label of the in-flight archive.org fetch, for the "working" screen.
    pub fn net_job_label(&self) -> Option<String> {
        self.net_job.as_ref().map(|j| j.label.clone())
    }

    /// Open the archive.org search screen.
    fn enter_archive(&mut self) {
        self.screen = Screen::ArchiveSearch;
    }

    fn on_archive_search_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Esc => self.screen = Screen::Menu,
            KeyCode::Enter => {
                let query = self.archive_query.text().trim().to_string();
                if query.is_empty() {
                    self.notice = Some("Type something to search for first.".to_string());
                    return;
                }
                self.net_target = Screen::ArchiveResults;
                self.net_job = Some(NetJob::start(NetRequest::Search { query, rows: 60 }));
                self.screen = Screen::ArchiveFetching;
            }
            _ => edit_input(&mut self.archive_query, code, mods),
        }
    }

    fn on_archive_fetching_key(&mut self, code: KeyCode) {
        // The worker can't be interrupted, but the user can walk away from it.
        if matches!(code, KeyCode::Esc) {
            self.net_job = None;
            self.screen = if self.net_target == Screen::ArchiveResults {
                Screen::ArchiveSearch
            } else {
                Screen::ArchiveResults
            };
        }
    }

    /// A background search/metadata fetch finished — route to its results screen.
    fn finalize_net_fetch(&mut self) {
        let Some(outcome) = self.net_job.take().and_then(|j| j.outcome) else {
            return;
        };
        match outcome {
            Ok(NetResult::Search(hits)) => {
                self.archive_hits = hits;
                self.archive_hits_state
                    .select((!self.archive_hits.is_empty()).then_some(0));
                if self.archive_hits.is_empty() {
                    self.notice = Some("No matching items on archive.org.".to_string());
                }
                // Enrich each result with its real importable-image count in the
                // background, so items with nothing to import are flagged before
                // the user drills into them.
                let ids: Vec<String> =
                    self.archive_hits.iter().map(|h| h.identifier.clone()).collect();
                self.archive_counts = vec![CountState::Pending; ids.len()];
                self.count_job = (!ids.is_empty()).then(|| CountJob::start(ids));
                self.screen = Screen::ArchiveResults;
            }
            Ok(NetResult::Files(files)) => {
                self.archive_files = files;
                self.archive_files_state
                    .select((!self.archive_files.is_empty()).then_some(0));
                if self.archive_files.is_empty() {
                    self.notice = Some(
                        "This item has no disk images or archives — only scans/metadata.".to_string(),
                    );
                }
                self.screen = Screen::ArchiveFiles;
            }
            Err(err) => {
                self.notice = Some(format!("archive.org: {err}"));
                self.screen = if self.net_target == Screen::ArchiveResults {
                    Screen::ArchiveSearch
                } else {
                    Screen::ArchiveResults
                };
            }
        }
    }

    fn on_archive_results_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Left => self.screen = Screen::ArchiveSearch,
            KeyCode::Up | KeyCode::Char('k') => {
                move_list(&mut self.archive_hits_state, self.archive_hits.len(), -1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                move_list(&mut self.archive_hits_state, self.archive_hits.len(), 1)
            }
            KeyCode::Enter | KeyCode::Right => {
                if let Some(hit) = self
                    .archive_hits_state
                    .selected()
                    .and_then(|i| self.archive_hits.get(i))
                {
                    let id = hit.identifier.clone();
                    self.archive_item_title = if hit.title.is_empty() {
                        hit.identifier.clone()
                    } else {
                        hit.title.clone()
                    };
                    self.net_target = Screen::ArchiveFiles;
                    self.net_job = Some(NetJob::start(NetRequest::Files { identifier: id }));
                    self.screen = Screen::ArchiveFetching;
                }
            }
            _ => {}
        }
    }

    fn on_archive_files_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Left => self.screen = Screen::ArchiveResults,
            KeyCode::Up | KeyCode::Char('k') => {
                move_list(&mut self.archive_files_state, self.archive_files.len(), -1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                move_list(&mut self.archive_files_state, self.archive_files.len(), 1)
            }
            KeyCode::Enter | KeyCode::Right => {
                if let Some(file) = self
                    .archive_files_state
                    .selected()
                    .and_then(|i| self.archive_files.get(i))
                    .cloned()
                {
                    let dest = self.core.paths.library_dir.join(&self.lib_subpath);
                    let clip = self.clip_dir();
                    self.download_job = Some(DownloadJob::start(file, dest, clip));
                    self.screen = Screen::ArchiveDownloading;
                }
            }
            _ => {}
        }
    }

    /// A download finished — catalogue images into the library, or stage loose
    /// files (from a game's zip) into the clipboard to paste into an image.
    fn finalize_download(&mut self) {
        let Some(result) = self.download_job.take().and_then(|j| j.result) else {
            return;
        };
        match result {
            Ok(DlOutcome::Images { paths, from_zip }) => {
                let scanned = gwm_core::library::scan_import(
                    &self.core.catalog,
                    &self.core.paths.library_dir,
                );
                let names: Vec<String> = paths
                    .iter()
                    .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
                    .collect();
                let summary = match names.as_slice() {
                    [one] => one.clone(),
                    many => format!("{} disk images", many.len()),
                };
                self.notice = Some(match &scanned {
                    Ok(_) if from_zip => format!("Unpacked {summary} from the archive into the library."),
                    Ok(_) => format!("Imported {summary} into the library."),
                    Err(err) => format!("Imported {summary}, but the catalog scan failed: {err}"),
                });
                let _ = self.reload_library();
                self.lib_filter.clear();
                self.lib_filtering = false;
                self.screen = Screen::Library;
            }
            Ok(DlOutcome::LooseFiles { staged, source }) => {
                let n = staged.len();
                for (name, path) in staged {
                    self.clipboard.push(ClipItem { name, path });
                }
                if self.clip_state.selected().is_none() && !self.clipboard.is_empty() {
                    self.clip_state.select(Some(0));
                }
                self.notice = Some(format!(
                    "{source} had no disk image — its {n} loose file(s) are on the clipboard. \
                     Open or create an image, press b to browse, then Tab → p to paste them in."
                ));
                self.screen = Screen::ArchiveFiles;
            }
            Ok(DlOutcome::Saved { path, note }) => {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string();
                let _ = self.reload_library();
                self.notice = Some(format!("{name} — {note}."));
                self.screen = Screen::ArchiveFiles;
            }
            Err(err) => {
                self.notice = Some(format!("Download failed: {err}"));
                self.screen = Screen::ArchiveFiles;
            }
        }
    }

    fn on_menu_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => {
                self.menu_index = self.menu_index.checked_sub(1).unwrap_or(MENU_ITEMS.len() - 1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.menu_index = (self.menu_index + 1) % MENU_ITEMS.len();
            }
            KeyCode::Enter => match self.menu_index {
                0 => self.enter_read_flow(),
                1 => self.enter_write_flow(),
                2 => self.reset_device(),
                3 => self.test_rpm(),
                4 => self.start_clean(),
                5 => self.enter_library(),
                6 => self.enter_create_flow(),
                7 => self.enter_archive(),
                8 => self.enter_tools(),
                9 => {
                    self.settings_index = 0;
                    self.settings_editing = false;
                    self.screen = Screen::Settings;
                }
                10 => self.should_quit = true,
                _ => {}
            },
            _ => {}
        }
    }

    /// Reset the attached Greaseweazle to power-on defaults (`gw reset`). Quick
    /// and non-destructive to any disk, so it runs inline with a status notice.
    fn reset_device(&mut self) {
        if !self.gw_ready() {
            return;
        }
        self.notice = Some(match gwm_core::device::reset() {
            Ok(()) => "Greaseweazle reset to power-on defaults.".to_string(),
            Err(e) => format!("gw reset failed: {e}"),
        });
    }

    /// Kick off a background `gw rpm` measurement of the default drive. The menu
    /// shows "testing…" next to the item until the reading replaces it.
    fn test_rpm(&mut self) {
        if !self.gw_ready() {
            return;
        }
        if self.rpm_job.is_some() {
            return; // already measuring
        }
        self.rpm_result = None;
        self.rpm_job = Some(RpmJob::start(self.core.settings.default_drive.clone()));
    }

    /// Text shown next to the "Test drive RPM" menu item: nothing until first
    /// used, then "testing…", then the reading or a short error.
    pub fn rpm_menu_note(&self) -> Option<String> {
        if self.rpm_job.is_some() {
            return Some("testing…".to_string());
        }
        match self.rpm_result.as_deref() {
            Some(text) => Some(text.to_string()),
            None => None,
        }
    }

    fn gw_ready(&mut self) -> bool {
        if !self.core.gw.available {
            self.notice = Some("gw is unavailable — check the footer.".to_string());
            return false;
        }
        true
    }

    fn enter_read_flow(&mut self) {
        if !self.gw_ready() {
            return;
        }
        if self.formats.is_empty() {
            self.formats = formats::list_formats();
        }
        if self.formats.is_empty() {
            self.notice = Some("Could not read the format list from gw.".to_string());
            return;
        }
        self.flow = Flow::Read;
        self.format_filter.clear();
        self.format_state.select(Some(0));
        self.screen = Screen::FormatPicker;
    }

    fn enter_write_flow(&mut self) {
        if !self.gw_ready() {
            return;
        }
        if self.library.is_empty() {
            self.notice = Some("No images in your library to write.".to_string());
            return;
        }
        self.flow = Flow::Write;
        self.write_erase = false;
        self.write_state.select(Some(0));
        self.screen = Screen::WriteSource;
    }

    /// Library entries matching the current filter (name / format / system / tag).
    fn lib_base(&self) -> PathBuf {
        self.core.paths.library_dir.join(&self.lib_subpath)
    }

    /// Every ancestor directory of a catalogued item, under the library root.
    /// A folder in this set is known to lead to at least one image, so it's worth
    /// showing in the folder view (unlike unpacked tool folders full of `.dll`s).
    fn catalogued_dirs(&self) -> HashSet<PathBuf> {
        let root = self.core.paths.library_dir.as_path();
        let mut dirs = HashSet::new();
        for item in &self.library {
            let mut cur = Path::new(&item.path).parent();
            while let Some(dir) = cur {
                if !dir.starts_with(root) || dir == root {
                    break;
                }
                dirs.insert(dir.to_path_buf());
                cur = dir.parent();
            }
        }
        dirs
    }

    /// Folder-aware rows for the current directory: `..`, sub-folders, then the
    /// media files that live directly in this folder (respecting the filter).
    pub fn library_rows(&self) -> Vec<LibRow> {
        let base = self.lib_base();
        let mut rows = Vec::new();
        if !self.lib_subpath.as_os_str().is_empty() {
            rows.push(LibRow::Parent);
        }

        let at_root = self.lib_subpath.as_os_str().is_empty();
        // Directories that (recursively) contain a catalogued image, so we can
        // hide foreign folders — unpacked tools, source trees, etc. — that the
        // user happens to keep inside the store. Built from the catalog, which
        // scan_import has already refreshed by the time we render.
        let image_dirs = self.catalogued_dirs();
        let mut folders: Vec<String> = Vec::new();
        if let Ok(read) = std::fs::read_dir(&base) {
            for entry in read.flatten() {
                if entry.path().is_dir() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        continue;
                    }
                    // The app's own backup folder sits at the store root; don't
                    // show it as a browsable library folder.
                    if at_root && name == "originals" {
                        continue;
                    }
                    // Show a folder only if it leads to catalogued images or is
                    // still empty (a freshly made organising folder). This keeps
                    // tool/junk folders (full of non-image files) out of view.
                    let path = base.join(&name);
                    if !image_dirs.contains(&path) && !dir_is_empty(&path) {
                        continue;
                    }
                    folders.push(name);
                }
            }
        }
        folders.sort_by_key(|s| s.to_lowercase());
        rows.extend(folders.into_iter().map(LibRow::Folder));

        let needle = self.lib_filter.to_lowercase();
        for item in &self.library {
            if Path::new(&item.path).parent() != Some(base.as_path()) {
                continue;
            }
            // A KryoFlux capture is one `.raw` file per track/side; show only the
            // track-0/side-0 file (browsing it decodes the whole disk via hxcfe)
            // and hide the rest so one disk isn't dozens of rows.
            if is_hidden_flux_track(&item_file_name(item)) {
                continue;
            }
            if !needle.is_empty() {
                let name = item_file_name(item).to_lowercase();
                let hit = name.contains(&needle)
                    || item.format.as_deref().unwrap_or("").to_lowercase().contains(&needle)
                    || item.tags.iter().any(|t| t.to_lowercase().contains(&needle));
                if !hit {
                    continue;
                }
            }
            rows.push(LibRow::File(item.id));
        }
        rows
    }

    fn selected_row(&self) -> Option<LibRow> {
        self.lib_state
            .selected()
            .and_then(|i| self.library_rows().into_iter().nth(i))
    }

    /// The catalogued file under the cursor, if the cursor is on a file.
    fn selected_file(&self) -> Option<MediaItem> {
        match self.selected_row()? {
            LibRow::File(id) => self.library.iter().find(|it| it.id == id).cloned(),
            _ => None,
        }
    }

    fn selected_library(&self) -> Option<(i64, String, String)> {
        self.selected_file()
            .map(|it| (it.id, it.path.clone(), item_file_name(&it)))
    }

    /// Filename of the highlighted file (empty if the cursor is on a folder).
    pub fn selected_name(&self) -> String {
        self.selected_file()
            .map(|it| item_file_name(&it))
            .unwrap_or_default()
    }

    /// Act on the highlighted row: descend into a folder or go up via `..`.
    fn library_open_selected(&mut self) {
        match self.selected_row() {
            Some(LibRow::Parent) => {
                self.lib_subpath.pop();
                self.lib_filter.clear();
                self.lib_state.select(Some(0));
            }
            Some(LibRow::Folder(name)) => {
                self.lib_subpath.push(name);
                self.lib_filter.clear();
                self.lib_state.select(Some(0));
            }
            _ => {}
        }
    }

    fn on_library_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if self.lib_filtering {
            match code {
                KeyCode::Enter | KeyCode::Esc | KeyCode::Down => self.lib_filtering = false,
                KeyCode::Backspace => {
                    self.lib_filter.pop();
                    self.clamp_lib_selection();
                }
                KeyCode::Char(c) if is_typing(mods) => {
                    self.lib_filter.push(c);
                    self.clamp_lib_selection();
                }
                _ => {}
            }
            return;
        }

        match code {
            KeyCode::Char('q') => self.screen = Screen::Menu,
            KeyCode::Esc => {
                if !self.lib_filter.is_empty() {
                    self.lib_filter.clear();
                    self.clamp_lib_selection();
                } else if self.lib_subpath.as_os_str().is_empty() {
                    self.screen = Screen::Menu;
                } else {
                    self.lib_subpath.pop();
                    self.lib_state.select(Some(0));
                }
            }
            KeyCode::Backspace | KeyCode::Left => {
                if !self.lib_subpath.as_os_str().is_empty() {
                    self.lib_subpath.pop();
                    self.lib_state.select(Some(0));
                }
            }
            KeyCode::Enter | KeyCode::Right => self.library_open_selected(),
            KeyCode::Char('/') => self.lib_filtering = true,
            // Shift+M moves the selected file into a folder. Terminals encode it
            // as either `Char('M')` or `Char('m')`+SHIFT, so accept both before
            // the bare `m` (new folder) arm below.
            KeyCode::Char('M') => self.enter_library_move(),
            KeyCode::Char('m') if mods.contains(KeyModifiers::SHIFT) => self.enter_library_move(),
            KeyCode::Char('m') => {
                self.folder_input.set(String::new());
                self.screen = Screen::NewFolder;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let n = self.library_rows().len();
                move_list(&mut self.lib_state, n, -1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let n = self.library_rows().len();
                move_list(&mut self.lib_state, n, 1);
            }
            KeyCode::Char('v') => self.verify_selected(),
            KeyCode::Char('b') => self.start_browse(),
            KeyCode::Char('g') => self.start_gotek(),
            KeyCode::Char('f') => self.reformat_selected(),
            KeyCode::Char('d') => {
                if self.selected_file().is_some() {
                    self.delete_file = false;
                    self.screen = Screen::LibraryConfirmDelete;
                }
            }
            KeyCode::Char('r') => {
                if let Some((_, _, name)) = self.selected_library() {
                    self.rename_input.set(name);
                    self.screen = Screen::LibraryRename;
                }
            }
            KeyCode::Char('n') => {
                if let Some(item) = self.selected_file() {
                    self.notes_id = item.id;
                    self.notes_input.set(item.notes.clone().unwrap_or_default());
                    self.screen = Screen::EditNotes;
                }
            }
            KeyCode::Char('h') => {
                if let Some(item) = self.selected_file() {
                    let name = item_file_name(&item);
                    self.open_hex(&PathBuf::from(item.path), name, Screen::Library, None);
                }
            }
            _ => {}
        }
    }

    fn on_new_folder_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Esc => self.screen = Screen::Library,
            KeyCode::Enter => self.do_create_folder(),
            _ => edit_input(&mut self.folder_input, code, mods),
        }
    }

    fn do_create_folder(&mut self) {
        let name = self.folder_input.text().trim().replace(['/', '\\'], "_");
        if name.is_empty() {
            self.screen = Screen::Library;
            return;
        }
        let path = self.lib_base().join(&name);
        match std::fs::create_dir_all(&path) {
            Ok(()) => self.notice = Some(format!("Created folder “{name}”")),
            Err(err) => self.notice = Some(format!("Could not create folder: {err}")),
        }
        self.screen = Screen::Library;
    }

    /// Load a file's bytes into the hex viewer (capped so huge images are safe).
    /// Pass `edit = Some(entry)` to make it an editable file-in-image whose bytes
    /// are written back through the driver on save; `None` is a read-only view.
    fn open_hex(&mut self, path: &Path, title: String, return_to: Screen, edit: Option<FileEntry>) {
        const MAX: usize = 32 * 1024 * 1024;
        match std::fs::read(path) {
            Ok(mut data) => {
                data.truncate(MAX);
                self.hex_data = data;
                self.hex_offset = 0;
                self.hex_title = title;
                self.hex_return = return_to;
                self.hex_cursor = 0;
                self.hex_nibble = false;
                self.hex_ascii = false;
                self.hex_edit = false;
                self.hex_dirty = false;
                self.hex_confirm_discard = false;
                self.hex_entry = edit;
                self.hex_temp = path.to_path_buf();
                self.screen = Screen::HexView;
            }
            Err(err) => self.notice = Some(format!("Could not read file: {err}")),
        }
    }

    /// Whether the current hex view can be edited (a file-in-image, not a raw
    /// image). Used by the UI to show the edit affordance.
    pub fn hex_editable(&self) -> bool {
        self.hex_entry.is_some()
    }

    // --- format-preserving text editor ----------------------------------

    /// Open a file-in-image for text editing. Decodes with the codec for the
    /// image's filesystem (v1: raw/Latin-1), remembering line-ending + trailing
    /// state so an unedited save is byte-identical.
    fn open_text(&mut self, path: &Path, title: String, entry: FileEntry) {
        const MAX: usize = 8 * 1024 * 1024;
        let mut bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(err) => {
                self.notice = Some(format!("Could not read file: {err}"));
                return;
            }
        };
        bytes.truncate(MAX);
        let fs = self.browse_driver;
        // A Commodore program file is tokenised BASIC, not text — showing raw
        // bytes is line-link/token noise. When it parses as BASIC, show a
        // read-only LISTing instead; otherwise fall back to the text editor.
        let listing = (fs == FsKind::Cbm)
            .then(|| gwm_core::cbm_basic::detokenize(&bytes))
            .flatten();
        let (lines, eol, trailing, readonly, title) = match listing {
            Some(text) => (
                text.lines().map(|l| l.chars().collect()).collect(),
                gwm_core::textedit::Eol::Lf,
                true,
                true,
                format!("{title}  ·  BASIC listing (read-only)"),
            ),
            None => {
                let doc =
                    gwm_core::textedit::TextDoc::open(&bytes, gwm_core::textedit::codec_for_fs(fs));
                let lines = doc.lines.iter().map(|l| l.chars().collect()).collect();
                (lines, doc.eol(), doc.trailing_newline(), false, title)
            }
        };
        self.text_readonly = readonly;
        self.text_lines = lines;
        if self.text_lines.is_empty() {
            self.text_lines.push(Vec::new());
        }
        self.text_eol = eol;
        self.text_trailing = trailing;
        self.text_fs = fs;
        self.text_title = title;
        self.text_return = Screen::Browse;
        self.text_row = 0;
        self.text_col = 0;
        self.text_scroll = 0;
        self.text_dirty = false;
        self.text_confirm_discard = false;
        self.text_entry = Some(entry);
        self.text_temp = path.to_path_buf();
        self.screen = Screen::TextEdit;
    }

    fn on_text_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if mods.contains(KeyModifiers::CONTROL) {
            if code == KeyCode::Char('s') && !self.text_readonly {
                self.text_save();
            }
            return;
        }
        // Any key other than Esc disarms the "second Esc discards" prompt.
        if code != KeyCode::Esc {
            self.text_confirm_discard = false;
        }
        let last = self.text_lines.len().saturating_sub(1);
        match code {
            KeyCode::Esc => {
                if self.text_dirty && !self.text_confirm_discard {
                    self.text_confirm_discard = true;
                    self.notice =
                        Some("Unsaved changes — Esc again to discard, Ctrl-S to save.".to_string());
                } else {
                    self.text_confirm_discard = false;
                    self.screen = self.text_return;
                }
            }
            KeyCode::Up => {
                if self.text_row > 0 {
                    self.text_row -= 1;
                    self.text_col = self.text_col.min(self.text_lines[self.text_row].len());
                }
            }
            KeyCode::Down => {
                if self.text_row < last {
                    self.text_row += 1;
                    self.text_col = self.text_col.min(self.text_lines[self.text_row].len());
                }
            }
            KeyCode::Left => {
                if self.text_col > 0 {
                    self.text_col -= 1;
                } else if self.text_row > 0 {
                    self.text_row -= 1;
                    self.text_col = self.text_lines[self.text_row].len();
                }
            }
            KeyCode::Right => {
                if self.text_col < self.text_lines[self.text_row].len() {
                    self.text_col += 1;
                } else if self.text_row < last {
                    self.text_row += 1;
                    self.text_col = 0;
                }
            }
            KeyCode::Home => self.text_col = 0,
            KeyCode::End => self.text_col = self.text_lines[self.text_row].len(),
            KeyCode::PageUp => {
                let step = self.text_rows.max(1);
                self.text_row = self.text_row.saturating_sub(step);
                self.text_col = self.text_col.min(self.text_lines[self.text_row].len());
            }
            KeyCode::PageDown => {
                let step = self.text_rows.max(1);
                self.text_row = (self.text_row + step).min(last);
                self.text_col = self.text_col.min(self.text_lines[self.text_row].len());
            }
            KeyCode::Enter if !self.text_readonly => {
                let tail = self.text_lines[self.text_row].split_off(self.text_col);
                self.text_lines.insert(self.text_row + 1, tail);
                self.text_row += 1;
                self.text_col = 0;
                self.text_dirty = true;
            }
            KeyCode::Backspace if !self.text_readonly => {
                if self.text_col > 0 {
                    self.text_lines[self.text_row].remove(self.text_col - 1);
                    self.text_col -= 1;
                    self.text_dirty = true;
                } else if self.text_row > 0 {
                    let cur = self.text_lines.remove(self.text_row);
                    self.text_row -= 1;
                    self.text_col = self.text_lines[self.text_row].len();
                    self.text_lines[self.text_row].extend(cur);
                    self.text_dirty = true;
                }
            }
            KeyCode::Delete if !self.text_readonly => {
                if self.text_col < self.text_lines[self.text_row].len() {
                    self.text_lines[self.text_row].remove(self.text_col);
                    self.text_dirty = true;
                } else if self.text_row < last {
                    let next = self.text_lines.remove(self.text_row + 1);
                    self.text_lines[self.text_row].extend(next);
                    self.text_dirty = true;
                }
            }
            KeyCode::Tab if !self.text_readonly => {
                self.text_lines[self.text_row].insert(self.text_col, '\t');
                self.text_col += 1;
                self.text_dirty = true;
            }
            KeyCode::Char(c) if is_typing(mods) && !self.text_readonly => {
                self.text_lines[self.text_row].insert(self.text_col, c);
                self.text_col += 1;
                self.text_dirty = true;
            }
            _ => {}
        }
        self.keep_text_cursor_visible();
    }

    /// Adjust the scroll so the cursor row stays within the last-rendered window.
    fn keep_text_cursor_visible(&mut self) {
        if self.text_row < self.text_scroll {
            self.text_scroll = self.text_row;
        }
        let rows = self.text_rows.max(1);
        if self.text_row >= self.text_scroll + rows {
            self.text_scroll = self.text_row + 1 - rows;
        }
    }

    /// Re-encode the buffer (preserving EOL + trailing newline via the codec) and
    /// write it back into the image through the same path the hex editor uses.
    fn text_save(&mut self) {
        let Some(entry) = self.text_entry.clone() else {
            return;
        };
        if !self.text_dirty {
            self.notice = Some("No changes to save.".to_string());
            return;
        }
        if let Err(err) = self.backup_original(&entry) {
            self.notice = Some(format!("Could not stash original: {err}"));
            return;
        }
        let lines: Vec<String> = self
            .text_lines
            .iter()
            .map(|cs| cs.iter().collect())
            .collect();
        let doc = gwm_core::textedit::TextDoc::from_parts(lines, self.text_eol, self.text_trailing);
        let bytes = doc.to_bytes(gwm_core::textedit::codec_for_fs(self.text_fs));
        if let Err(err) = std::fs::write(&self.text_temp, &bytes) {
            self.notice = Some(format!("Could not write temp file: {err}"));
            return;
        }
        match self.write_back(&entry, &self.text_temp.clone()) {
            Ok(()) => {
                self.text_dirty = false;
                self.text_confirm_discard = false;
                self.browse_originals.insert(entry.name.clone());
                self.after_image_modified();
                self.notice = Some(format!("Saved {} back into the image.", entry.name));
            }
            Err(err) => self.notice = Some(format!("Save failed: {err}")),
        }
    }

    fn hex_last_line(&self) -> usize {
        let len = self.hex_data.len();
        if len == 0 {
            0
        } else {
            (len - 1) / 16 * 16
        }
    }

    fn on_hex_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Ctrl-S saves an edited file-in-image, whatever the current sub-mode.
        if self.hex_edit && mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('s') {
            self.hex_save();
            return;
        }
        if self.hex_edit {
            self.on_hex_edit_key(code);
        } else {
            self.on_hex_view_key(code);
        }
    }

    /// Read-only scrolling mode (also the mode for raw-image views).
    fn on_hex_view_key(&mut self, code: KeyCode) {
        let last = self.hex_last_line();
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = self.hex_return,
            KeyCode::Up | KeyCode::Char('k') => self.hex_offset = self.hex_offset.saturating_sub(16),
            KeyCode::Down | KeyCode::Char('j') => {
                self.hex_offset = (self.hex_offset + 16).min(last)
            }
            KeyCode::PageUp => self.hex_offset = self.hex_offset.saturating_sub(16 * 16),
            KeyCode::PageDown => self.hex_offset = (self.hex_offset + 16 * 16).min(last),
            KeyCode::Home => self.hex_offset = 0,
            KeyCode::End => self.hex_offset = last,
            // Only files-in-images can be edited; raw-image views stay read-only.
            KeyCode::Char('e') if self.hex_entry.is_some() && !self.hex_data.is_empty() => {
                self.hex_edit = true;
                self.hex_confirm_discard = false;
                self.hex_cursor = self.hex_cursor.min(self.hex_data.len() - 1);
                self.hex_nibble = false;
                self.notice = Some(
                    "Edit mode — type hex over bytes, Ctrl-S saves, Esc leaves.".to_string(),
                );
                self.hex_ensure_visible();
            }
            _ => {}
        }
    }

    /// Overtype editing: hex digits overwrite the byte under the cursor; the file
    /// length never changes (write-over only).
    fn on_hex_edit_key(&mut self, code: KeyCode) {
        let len = self.hex_data.len();
        if len == 0 {
            self.hex_edit = false;
            return;
        }
        match code {
            KeyCode::Esc => {
                if self.hex_dirty && !self.hex_confirm_discard {
                    self.hex_confirm_discard = true;
                    self.notice =
                        Some("Unsaved edits — Ctrl-S to save, Esc again to discard.".to_string());
                } else {
                    // Leave edit mode back to plain viewing (keeps unsaved buffer
                    // only if they re-enter; discarding here means the temp still
                    // holds saved bytes, the in-memory buffer is dropped on exit).
                    self.hex_edit = false;
                    self.hex_confirm_discard = false;
                    self.screen = self.hex_return;
                }
            }
            KeyCode::Left => {
                self.hex_nibble = false;
                self.hex_cursor = self.hex_cursor.saturating_sub(1);
                self.hex_ensure_visible();
            }
            KeyCode::Right => {
                self.hex_nibble = false;
                self.hex_cursor = (self.hex_cursor + 1).min(len - 1);
                self.hex_ensure_visible();
            }
            KeyCode::Up => {
                self.hex_cursor = self.hex_cursor.saturating_sub(16);
                self.hex_ensure_visible();
            }
            KeyCode::Down => {
                if self.hex_cursor + 16 < len {
                    self.hex_cursor += 16;
                }
                self.hex_ensure_visible();
            }
            KeyCode::Home => {
                self.hex_cursor -= self.hex_cursor % 16;
                self.hex_nibble = false;
                self.hex_ensure_visible();
            }
            KeyCode::End => {
                self.hex_cursor = (self.hex_cursor - self.hex_cursor % 16 + 15).min(len - 1);
                self.hex_nibble = false;
                self.hex_ensure_visible();
            }
            KeyCode::PageUp => {
                let step = self.hex_rows.max(1) * 16;
                self.hex_cursor = self.hex_cursor.saturating_sub(step);
                self.hex_ensure_visible();
            }
            KeyCode::PageDown => {
                let step = self.hex_rows.max(1) * 16;
                self.hex_cursor = (self.hex_cursor + step).min(len - 1);
                self.hex_ensure_visible();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.hex_ascii = !self.hex_ascii;
                self.hex_nibble = false;
            }
            // ASCII column: any printable character overwrites the byte.
            KeyCode::Char(c) if self.hex_ascii && (0x20..0x7f).contains(&(c as u32)) => {
                self.hex_data[self.hex_cursor] = c as u8;
                self.hex_dirty = true;
                self.hex_confirm_discard = false;
                if self.hex_cursor + 1 < len {
                    self.hex_cursor += 1;
                }
                self.hex_ensure_visible();
            }
            // Hex column: hex digits overwrite one nibble at a time.
            KeyCode::Char(c) if !self.hex_ascii && c.is_ascii_hexdigit() => {
                let nib = (c.to_digit(16).unwrap()) as u8;
                let byte = &mut self.hex_data[self.hex_cursor];
                if self.hex_nibble {
                    *byte = (*byte & 0xf0) | nib;
                    // Low nibble done: advance to the next byte's high nibble.
                    self.hex_nibble = false;
                    if self.hex_cursor + 1 < len {
                        self.hex_cursor += 1;
                    }
                } else {
                    *byte = (*byte & 0x0f) | (nib << 4);
                    self.hex_nibble = true;
                }
                self.hex_dirty = true;
                self.hex_confirm_discard = false;
                self.hex_ensure_visible();
            }
            _ => {}
        }
    }

    /// Scroll `hex_offset` so the cursor row is within the visible window.
    fn hex_ensure_visible(&mut self) {
        let rows = self.hex_rows.max(1);
        let cur_line = self.hex_cursor / 16 * 16;
        if cur_line < self.hex_offset {
            self.hex_offset = cur_line;
        } else if cur_line >= self.hex_offset + rows * 16 {
            self.hex_offset = cur_line - (rows - 1) * 16;
        }
    }

    /// Write the edited buffer back into the image (delete + re-insert the same
    /// name), stashing a pristine copy the first time so it can be restored.
    fn hex_save(&mut self) {
        let Some(entry) = self.hex_entry.clone() else {
            return;
        };
        if !self.hex_dirty {
            self.notice = Some("No changes to save.".to_string());
            return;
        }
        // Stash the pristine original once, before the first write-back lands.
        if let Err(err) = self.backup_original(&entry) {
            self.notice = Some(format!("Could not stash original: {err}"));
            return;
        }
        // Persist the edited bytes to the temp file the driver inserts from.
        if let Err(err) = std::fs::write(&self.hex_temp, &self.hex_data) {
            self.notice = Some(format!("Could not write temp file: {err}"));
            return;
        }
        match self.write_back(&entry, &self.hex_temp.clone()) {
            Ok(()) => {
                self.hex_dirty = false;
                self.hex_confirm_discard = false;
                self.browse_originals.insert(entry.name.clone());
                self.after_image_modified();
                self.notice = Some(format!("Saved {} back into the image.", entry.name));
            }
            Err(err) => self.notice = Some(format!("Save failed: {err}")),
        }
    }

    /// Replace `entry` in the current image with the bytes at `src` (same name).
    fn write_back(&self, entry: &FileEntry, src: &Path) -> Result<()> {
        let fs = self.browse_fs();
        // Some files can't survive a delete+insert (Commodore REL would become a
        // PRG and lose its side sectors). For those the driver overwrites the
        // existing bytes in place — the hex editor only overtypes, so the length
        // is unchanged and nothing else needs rebuilding.
        if let Some(res) = fs.overwrite(&self.browse_image, entry, src) {
            return res.map_err(Into::into);
        }
        // Preserve the original file type/attributes across the rewrite (e.g. the
        // Commodore driver keeps SEQ/USR from being flattened to PRG).
        let name = fs.rewrite_name(&self.browse_image, entry);
        // Delete-then-insert overwrites cleanly across cpmtools/mtools/c1541 and
        // lets each tool rebuild any filesystem metadata/checksums.
        let _ = fs.delete(&self.browse_image, entry);
        fs.insert(&self.browse_image, src, &name, entry.user)?;
        Ok(())
    }

    // --- pristine-original backups --------------------------------------

    /// Folder holding pristine copies of edited files, keyed by image id. Lives
    /// under the portable store dir so backups travel with the rest of the data.
    fn originals_dir(&self) -> PathBuf {
        self.core.paths.originals_dir().join(self.browse_id.to_string())
    }

    /// Where the pristine copy of `name` lives (may not exist yet).
    fn original_path(&self, name: &str) -> PathBuf {
        // Flatten any slashes so the filename can't escape the folder.
        self.originals_dir().join(safe_host_name(name))
    }

    /// Save a pristine copy of `entry` the first time it is edited. No-op if one
    /// already exists (so the "original" stays the truly first-seen bytes).
    fn backup_original(&self, entry: &FileEntry) -> std::io::Result<()> {
        let dest = self.original_path(&entry.name);
        if dest.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(self.originals_dir())?;
        // The current temp file still holds the just-extracted (pristine) bytes
        // only before the first save; use the on-disk image as the source of
        // truth by extracting fresh into the backup path.
        match self.browse_fs().extract(&self.browse_image, entry, &dest) {
            Ok(()) => Ok(()),
            Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())),
        }
    }

    /// Restore the selected file from its stored pristine original.
    fn restore_original(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let src = self.original_path(&entry.name);
        if !src.exists() {
            self.notice = Some(format!("No stored original for {}.", entry.name));
            return;
        }
        match self.write_back(&entry, &src.clone()) {
            Ok(()) => {
                // Restoring returns the file to pristine; drop the backup so a
                // future edit re-captures a fresh original.
                let _ = std::fs::remove_file(&src);
                self.browse_originals.remove(&entry.name);
                self.after_image_modified();
                self.notice = Some(format!("Restored {} from original.", entry.name));
            }
            Err(err) => self.notice = Some(format!("Restore failed: {err}")),
        }
        self.open_browser();
    }

    /// Populate `browse_originals` from the backup folder for this image.
    fn load_originals(&mut self) {
        self.browse_originals.clear();
        if let Ok(rd) = std::fs::read_dir(self.originals_dir()) {
            for e in rd.flatten() {
                if let Some(n) = e.file_name().to_str() {
                    self.browse_originals.insert(n.to_string());
                }
            }
        }
    }

    fn on_edit_notes_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Esc => self.screen = Screen::Library,
            KeyCode::Enter => {
                let notes = self.notes_input.text().trim().to_string();
                let value = if notes.is_empty() { None } else { Some(notes.as_str()) };
                let _ = self.core.catalog.update_notes(self.notes_id, value);
                let _ = self.reload_library();
                self.notice = Some("Notes saved.".to_string());
                self.screen = Screen::Library;
            }
            _ => edit_input(&mut self.notes_input, code, mods),
        }
    }

    fn clamp_lib_selection(&mut self) {
        let len = self.library_rows().len();
        if len == 0 {
            self.lib_state.select(None);
            return;
        }
        let current = self.lib_state.selected().unwrap_or(0).min(len - 1);
        self.lib_state.select(Some(current));
    }

    fn verify_selected(&mut self) {
        if let Some(item) = self.selected_file() {
            let result = check_integrity(&item);
            self.verify_results.insert(item.id, result);
            self.notice = Some(format!("Integrity: {}", result.label()));
        }
    }

    fn on_library_delete_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => self.screen = Screen::Library,
            KeyCode::Char('f') => self.delete_file = !self.delete_file,
            KeyCode::Char('y') | KeyCode::Char('Y') => self.do_delete(),
            _ => {}
        }
    }

    fn do_delete(&mut self) {
        if let Some((id, path, name)) = self.selected_library() {
            let _ = self.core.catalog.delete(id);
            if self.delete_file {
                let _ = std::fs::remove_file(&path);
            }
            self.verify_results.remove(&id);
            let _ = self.reload_library();
            self.clamp_lib_selection();
            self.notice = Some(format!("Removed “{name}” from the library"));
        }
        self.screen = Screen::Library;
    }

    fn on_library_rename_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if code == KeyCode::Esc {
            self.screen = Screen::Library;
        } else if code == KeyCode::Enter {
            self.do_rename();
        } else {
            edit_input(&mut self.rename_input, code, mods);
        }
    }

    fn do_rename(&mut self) {
        let new_name = self.rename_input.text().trim().replace('/', "_");
        if new_name.is_empty() {
            self.notice = Some("Name cannot be empty.".to_string());
            self.screen = Screen::Library;
            return;
        }
        if let Some((id, old_path, _)) = self.selected_library() {
            let old = PathBuf::from(&old_path);
            let dir = old
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.core.paths.library_dir.clone());
            let new_path = dir.join(&new_name);
            if new_path.exists() {
                self.notice = Some("A file with that name already exists.".to_string());
            } else {
                match std::fs::rename(&old, &new_path) {
                    Ok(()) => {
                        let _ = self.core.catalog.update_path(id, &new_path.to_string_lossy());
                        let _ = self.reload_library();
                        self.notice = Some(format!("Renamed to “{new_name}”"));
                    }
                    Err(err) => self.notice = Some(format!("Rename failed: {err}")),
                }
            }
        }
        self.screen = Screen::Library;
    }

    /// Destination folders a file may be moved into: the store root plus every
    /// catalogued or empty sub-folder (foreign tool/junk folders are hidden, same
    /// as the library view), minus `exclude` (the file's current folder). Sorted
    /// with the root first, then by relative path.
    fn move_target_dirs(&self, exclude: Option<&Path>) -> Vec<PathBuf> {
        let root = self.core.paths.library_dir.clone();
        let image_dirs = self.catalogued_dirs();
        let mut out = Vec::new();
        if exclude != Some(root.as_path()) {
            out.push(root.clone());
        }
        // Walk sub-folders with a depth cap, matching library.rs's bounded scan.
        let mut stack = vec![(root.clone(), 0u32)];
        while let Some((dir, depth)) = stack.pop() {
            if depth >= 8 {
                continue;
            }
            let Ok(read) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in read.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || (dir == root && name == "originals") {
                    continue;
                }
                // Only offer folders that lead to catalogued images or are empty,
                // so unpacked tools and source trees don't clutter the picker.
                let catalogued = image_dirs.contains(&path);
                if (catalogued || dir_is_empty(&path)) && Some(path.as_path()) != exclude {
                    out.push(path.clone());
                }
                if catalogued {
                    stack.push((path, depth + 1)); // empty folders have nothing below
                }
            }
        }
        out.sort_by_key(|p| p.strip_prefix(&root).unwrap_or(p).to_string_lossy().to_lowercase());
        out
    }

    /// Human label for a destination folder: the store root, or its path relative
    /// to the root.
    fn move_target_label(&self, dir: &Path) -> String {
        match dir.strip_prefix(&self.core.paths.library_dir) {
            Ok(rel) if rel.as_os_str().is_empty() => "the store root".to_string(),
            Ok(rel) => format!("“{}”", rel.to_string_lossy()),
            Err(_) => dir.to_string_lossy().into_owned(),
        }
    }

    /// Name of the file being moved, for the move picker's header.
    pub fn move_item_name(&self) -> Option<&str> {
        self.move_item.as_ref().map(|(_, _, name)| name.as_str())
    }

    /// Display string for a destination folder row in the move picker.
    pub fn move_target_display(&self, dir: &Path) -> String {
        match dir.strip_prefix(&self.core.paths.library_dir) {
            Ok(rel) if rel.as_os_str().is_empty() => "⌂  store root".to_string(),
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => dir.to_string_lossy().into_owned(),
        }
    }

    fn enter_library_move(&mut self) {
        let Some((id, path, name)) = self.selected_library() else {
            return;
        };
        let current = Path::new(&path).parent().map(Path::to_path_buf);
        let targets = self.move_target_dirs(current.as_deref());
        if targets.is_empty() {
            self.notice =
                Some("No other folder to move into — make one with “m” first.".to_string());
            return;
        }
        self.move_item = Some((id, path, name));
        self.move_targets = targets;
        self.move_state.select(Some(0));
        self.screen = Screen::LibraryMove;
    }

    fn on_library_move_key(&mut self, code: KeyCode) {
        let count = self.move_targets.len();
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.move_item = None;
                self.screen = Screen::Library;
            }
            KeyCode::Up | KeyCode::Char('k') => move_list(&mut self.move_state, count, -1),
            KeyCode::Down | KeyCode::Char('j') => move_list(&mut self.move_state, count, 1),
            KeyCode::Enter => self.do_move(),
            _ => {}
        }
    }

    fn do_move(&mut self) {
        let Some((id, old_path, name)) = self.move_item.clone() else {
            self.screen = Screen::Library;
            return;
        };
        let Some(dest) = self
            .move_state
            .selected()
            .and_then(|i| self.move_targets.get(i))
            .cloned()
        else {
            self.screen = Screen::Library;
            return;
        };
        let old = PathBuf::from(&old_path);
        let new_path = dest.join(&name);
        if new_path == old {
            self.screen = Screen::Library;
            return;
        }
        if new_path.exists() {
            self.notice = Some("A file with that name already exists there.".to_string());
        } else {
            match std::fs::rename(&old, &new_path) {
                Ok(()) => {
                    let _ = self.core.catalog.update_path(id, &new_path.to_string_lossy());
                    // Don't leave the stale content sidecar behind in the old folder.
                    let _ = std::fs::remove_file(old.with_extension("txt"));
                    let _ = self.reload_library();
                    let label = self.move_target_label(&dest);
                    self.notice = Some(format!("Moved “{name}” to {label}"));
                }
                Err(err) => self.notice = Some(format!("Move failed: {err}")),
            }
        }
        self.move_item = None;
        self.screen = Screen::Library;
    }

    fn on_format_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Editing a format's label takes over the keyboard until Enter/Esc.
        if self.format_editing.is_some() {
            self.on_format_label_edit_key(code, mods);
            return;
        }
        match code {
            KeyCode::Esc => {
                self.screen = match self.flow {
                    Flow::Read => Screen::Menu,
                    Flow::Write => Screen::WriteSource,
                    Flow::Decode => Screen::Library,
                };
                if self.flow == Flow::Decode {
                    self.browse_master = None;
                }
            }
            // Ctrl+E: edit the selected format's label.
            KeyCode::Char('e') if mods.contains(KeyModifiers::CONTROL) => {
                self.begin_format_label_edit();
            }
            KeyCode::Up => {
                let n = self.filtered_formats().len();
                move_list(&mut self.format_state, n, -1);
            }
            KeyCode::Down => {
                let n = self.filtered_formats().len();
                move_list(&mut self.format_state, n, 1);
            }
            KeyCode::Backspace => {
                self.format_filter.pop();
                self.reset_format_selection();
            }
            KeyCode::Enter => {
                let choice = self
                    .format_state
                    .selected()
                    .and_then(|i| self.filtered_formats().get(i).map(|s| s.to_string()));
                if let Some(fmt) = choice {
                    self.record_recent_format(&fmt);
                    if self.flow == Flow::Decode {
                        // Decode the flux master with the chosen format and browse.
                        self.decode_and_open(&fmt);
                    } else {
                        self.chosen_format = fmt;
                        self.drive_index = self.default_drive_index();
                        self.screen = Screen::DrivePicker;
                    }
                }
            }
            KeyCode::Char(c) if is_typing(mods) => {
                self.format_filter.push(c);
                self.reset_format_selection();
            }
            _ => {}
        }
    }

    /// Whether the format picker is currently choosing a decode format for a
    /// flux master (vs a read/write format). Used by the renderer.
    pub fn is_decode_flow(&self) -> bool {
        self.flow == Flow::Decode
    }

    fn reset_format_selection(&mut self) {
        let any = !self.filtered_formats().is_empty();
        self.format_state.select(any.then_some(0));
    }

    /// Open the label editor for the currently-selected format, pre-filled with
    /// its effective label.
    fn begin_format_label_edit(&mut self) {
        let Some(fmt) = self
            .format_state
            .selected()
            .and_then(|i| self.filtered_formats().get(i).map(|s| s.to_string()))
        else {
            return;
        };
        let mut input = TextInput::new();
        input.set(self.format_label(&fmt));
        self.format_editing = Some((fmt, input));
    }

    fn on_format_label_edit_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Esc => self.format_editing = None,
            KeyCode::Enter => {
                if let Some((fmt, input)) = self.format_editing.take() {
                    let text = input.text().trim().to_string();
                    let labels = &mut self.core.settings.format_labels;
                    // Empty, or matching the generated guess → drop the override.
                    if text.is_empty() || text == formats::describe_format(&fmt) {
                        labels.remove(&fmt);
                        self.notice = Some(format!("Reset {fmt} to its default label."));
                    } else {
                        labels.insert(fmt.clone(), text);
                        self.notice = Some(format!("Saved label for {fmt}."));
                    }
                    let _ = self.core.save_settings();
                }
            }
            _ => {
                if let Some((_, input)) = self.format_editing.as_mut() {
                    edit_input(input, code, mods);
                }
            }
        }
    }

    fn on_drive_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Backspace => {
                self.screen = match self.flow {
                    Flow::Read => Screen::FormatPicker,
                    Flow::Write => Screen::WriteSource,
                    Flow::Decode => Screen::Library,
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.drive_index = self.drive_index.checked_sub(1).unwrap_or(DRIVE_OPTIONS.len() - 1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.drive_index = (self.drive_index + 1) % DRIVE_OPTIONS.len()
            }
            KeyCode::Enter => {
                self.chosen_drive = DRIVE_OPTIONS[self.drive_index].0.to_string();
                match self.flow {
                    Flow::Read => {
                        // Pre-tick hard-sectors for known hard-sectored formats;
                        // seed the track range from the format's standard cylinder
                        // count (so the fields show real numbers, not "default"),
                        // clearing any override from a previous read.
                        self.read_hard_sectors = is_hard_sectored(&self.chosen_format);
                        self.read_opt_row = 0;
                        let cyls = formats::format_cylinders(&self.chosen_format);
                        self.read_track_start = cyls.map(|_| 0);
                        self.read_track_end = cyls.map(|c| c.saturating_sub(1));
                        self.read_double_step = false;
                        self.screen = Screen::ReadOptions;
                    }
                    Flow::Write => self.screen = Screen::WriteConfirm,
                    // Decode never reaches the drive picker.
                    Flow::Decode => {}
                }
            }
            _ => {}
        }
    }

    fn on_name_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if code == KeyCode::Esc {
            self.screen = Screen::ReadOptions;
        } else if code == KeyCode::Enter {
            self.start_read();
        } else {
            edit_input(&mut self.name_input, code, mods);
        }
    }

    /// Rows on the read-options screen (in display order).
    const READ_OPT_ROWS: usize = 4; // hard-sectored, start, end, double-step

    fn on_read_options_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.screen = Screen::DrivePicker,
            KeyCode::Up | KeyCode::Char('k') => {
                self.read_opt_row =
                    self.read_opt_row.checked_sub(1).unwrap_or(Self::READ_OPT_ROWS - 1);
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.read_opt_row = (self.read_opt_row + 1) % Self::READ_OPT_ROWS;
            }
            KeyCode::Enter => {
                let default = self.default_name();
                self.name_input.set(default);
                self.screen = Screen::NameInput;
            }
            _ => self.adjust_read_opt(code),
        }
    }

    /// Change the value on the selected read-options row.
    fn adjust_read_opt(&mut self, code: KeyCode) {
        let toggle = matches!(
            code,
            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right | KeyCode::Char('x')
        );
        match self.read_opt_row {
            0 if toggle => self.read_hard_sectors = !self.read_hard_sectors,
            1 => adjust_track(&mut self.read_track_start, code),
            2 => adjust_track(&mut self.read_track_end, code),
            3 if toggle => self.read_double_step = !self.read_double_step,
            _ => {}
        }
    }

    fn on_write_source_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Backspace => self.screen = Screen::Menu,
            KeyCode::Up | KeyCode::Char('k') => move_list(&mut self.write_state, self.library.len(), -1),
            KeyCode::Down | KeyCode::Char('j') => move_list(&mut self.write_state, self.library.len(), 1),
            KeyCode::Enter => {
                let picked = self
                    .write_state
                    .selected()
                    .and_then(|i| self.library.get(i))
                    .map(|item| (item.path.clone(), item.format.clone()));
                if let Some((path, format)) = picked {
                    self.chosen_source = PathBuf::from(&path);
                    self.chosen_source_name = file_name(&self.chosen_source);
                    match format {
                        Some(fmt) => {
                            self.chosen_format = fmt;
                            self.drive_index = 0;
                            self.screen = Screen::DrivePicker;
                        }
                        None => {
                            if self.formats.is_empty() {
                                self.formats = formats::list_formats();
                            }
                            self.format_filter.clear();
                            self.format_state.select(Some(0));
                            self.screen = Screen::FormatPicker;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn on_write_confirm_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => self.screen = Screen::WriteSource,
            KeyCode::Char('e') => self.write_erase = !self.write_erase,
            KeyCode::Char('y') | KeyCode::Char('Y') => self.start_write(),
            _ => {}
        }
    }

    fn on_done_key(&mut self, code: KeyCode) {
        if matches!(code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
            self.read_job = None;
            self.read_outcome = None;
            self.write_job = None;
            self.write_outcome = None;
            self.screen = Screen::Menu;
        }
    }

    // --- read lifecycle --------------------------------------------------

    fn default_name(&self) -> String {
        let ext = formats::default_extension(&self.chosen_format);
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        format!("{}-{stamp}.{ext}", self.chosen_format.replace('.', "_"))
    }

    /// The output filename for the read (typed name, or the timestamped default).
    fn read_out_name(&self) -> String {
        let name = self.name_input.text().trim().replace('/', "_");
        if name.is_empty() {
            self.default_name()
        } else {
            name
        }
    }

    /// The `--tracks` spec for the current read-options selection.
    fn read_tracks(&self) -> Option<String> {
        gwm_core::device::read_tracks_arg(
            &self.chosen_format,
            self.read_track_start,
            self.read_track_end,
            self.read_double_step,
        )
    }

    /// The full `gw read …` command the current selections will run — shown on
    /// the name screen for documentation / reproducibility.
    pub fn read_command_preview(&self) -> String {
        let out = self.lib_base().join(self.read_out_name());
        let args = gwm_core::device::build_read_args(
            &self.chosen_format,
            &self.chosen_drive,
            None,
            self.read_hard_sectors,
            self.read_tracks().as_deref(),
            &out.to_string_lossy(),
        );
        format!("gw {}", args.join(" "))
    }

    fn start_read(&mut self) {
        let out_path = self.lib_base().join(self.read_out_name());
        // Push saved drive-timing tuning to the device before reading.
        let _ = gwm_core::device::apply_delays(&self.core.settings.tuning);
        let tracks = self.read_tracks();
        self.read_job = Some(ReadJob::start(
            self.chosen_format.clone(),
            self.chosen_drive.clone(),
            self.read_hard_sectors,
            tracks,
            out_path,
        ));
        self.read_outcome = None;
        self.screen = Screen::Reading;
    }

    /// Reading screen: reads are non-destructive, so Esc (or `c`) aborts safely.
    fn on_reading_key(&mut self, code: KeyCode) {
        if matches!(code, KeyCode::Esc | KeyCode::Char('c') | KeyCode::Char('C')) {
            if let Some(job) = self.read_job.as_mut() {
                if !job.cancelled {
                    job.request_cancel();
                    self.notice = Some("Cancelling read…".to_string());
                }
            }
        }
    }

    fn finalize_read(&mut self) {
        let outcome = {
            let job = match self.read_job.as_ref() {
                Some(job) => job,
                None => return,
            };
            if job.cancelled {
                // Drop the partial capture; a cancelled read isn't a device error.
                let _ = std::fs::remove_file(&job.out_path);
                Err("Read cancelled.".to_string())
            } else if job.succeeded() {
                let size = std::fs::metadata(&job.out_path)
                    .map(|m| m.len() as i64)
                    .unwrap_or(0);
                let item = NewMediaItem {
                    kind: MediaKind::Image,
                    path: job.out_path.to_string_lossy().into_owned(),
                    format: Some(job.format.clone()),
                    system: Some(formats::system_for_format(&job.format).to_string()),
                    size_bytes: size,
                    sha256: gwm_core::util::sha256_file(&job.out_path).ok(),
                    source: Source::Device,
                    remote_id: None,
                    tags: Vec::new(),
                    notes: job
                        .summary
                        .map(|(found, total, pct)| format!("{found}/{total} sectors ({pct}%)")),
                    fs_format: None,
                    fs_driver: None,
                };
                match self.core.catalog.insert(&item) {
                    Ok(_) => Ok(file_name(&job.out_path)),
                    Err(err) => Err(format!("read succeeded but cataloging failed: {err}")),
                }
            } else {
                let _ = std::fs::remove_file(&job.out_path);
                Err(job
                    .failed
                    .clone()
                    .unwrap_or_else(|| "read did not complete".to_string()))
            }
        };

        if outcome.is_ok() {
            let _ = self.reload_library();
        }
        self.read_outcome = Some(outcome);
        self.screen = Screen::ReadDone;
    }

    // --- write lifecycle -------------------------------------------------

    fn start_write(&mut self) {
        let in_path = self.chosen_source.to_string_lossy().into_owned();
        self.write_job = Some(WriteJob::start(
            self.chosen_format.clone(),
            self.chosen_drive.clone(),
            self.write_erase,
            in_path,
            self.chosen_source_name.clone(),
        ));
        self.write_outcome = None;
        self.screen = Screen::Writing;
    }

    fn finalize_write(&mut self) {
        let outcome = {
            let job = match self.write_job.as_ref() {
                Some(job) => job,
                None => return,
            };
            if job.succeeded() {
                Ok(job.source.clone())
            } else {
                Err(job
                    .failed
                    .clone()
                    .unwrap_or_else(|| "write did not complete".to_string()))
            }
        };
        self.write_outcome = Some(outcome);
        self.screen = Screen::WriteDone;
    }

    // --- tools installer -------------------------------------------------

    fn refresh_tool_status(&mut self) {
        self.tool_status = gwm_core::tools::TOOLS
            .iter()
            .map(|t| gwm_core::tools::installed(t.cmd))
            .collect();
        // Probe each tool's version in the background (spawns the tools, so it must
        // not block the render). Fills in `tool_versions` as results arrive.
        self.tool_versions = vec![VersionState::Pending; gwm_core::tools::TOOLS.len()];
        self.version_job = Some(VersionJob::start());
    }

    fn enter_tools(&mut self) {
        self.refresh_tool_status();
        self.tools_index = 0;
        self.screen = Screen::Tools;
    }

    fn on_tools_key(&mut self, code: KeyCode) {
        let count = gwm_core::tools::TOOLS.len();
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Menu,
            KeyCode::Up | KeyCode::Char('k') => {
                self.tools_index = self.tools_index.checked_sub(1).unwrap_or(count - 1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.tools_index = (self.tools_index + 1) % count;
            }
            KeyCode::Enter => self.activate_tool(),
            _ => {}
        }
    }

    fn activate_tool(&mut self) {
        let tool = gwm_core::tools::TOOLS[self.tools_index];
        if self.tool_status.get(self.tools_index).copied().unwrap_or(false) {
            self.notice = Some(format!("{} is already installed.", tool.label));
            return;
        }
        // Resolve how to install it on *this* system (apt/dnf/zypper/AUR + pipx).
        match gwm_core::tools::install_plan(&tool) {
            gwm_core::tools::InstallPlan::Run(cmd) => {
                // Don't touch the system yet: show a plain-English warning and make
                // the user agree first (see on_tool_confirm_key).
                self.tool_confirm = Some((self.tools_index, cmd));
                self.screen = Screen::ToolConfirm;
            }
            gwm_core::tools::InstallPlan::Manual { note, site } => {
                self.notice = Some(format!("{}: {note} {site}", tool.label));
            }
        }
    }

    fn on_tool_confirm_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some((idx, cmd)) = self.tool_confirm.take() {
                    // Run interactively — the run loop suspends the TUI so the
                    // package manager can prompt for a password. Remember the tool
                    // so we can tell if it actually landed once the command returns.
                    self.installing_tool = Some(idx);
                    self.run_interactive = Some(cmd);
                }
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                self.tool_confirm = None;
                self.screen = Screen::Tools;
                self.notice = Some("Install cancelled — nothing was changed.".to_string());
            }
            _ => {}
        }
    }

    /// Clean the (default) drive with a zig-zag pattern via `gw clean`.
    fn start_clean(&mut self) {
        if !self.core.gw.available {
            self.notice = Some("gw is unavailable — check the footer.".to_string());
            return;
        }
        let drive = self.core.settings.default_drive.clone();
        let label = format!("Cleaning drive {drive}");
        let cmd = format!("gw clean --drive={drive} 2>&1");
        self.install_return = Screen::Menu;
        self.install_job = Some(InstallJob::start(label, cmd));
        self.screen = Screen::Installing;
    }

    fn on_installing_key(&mut self, code: KeyCode) {
        let finished = self.install_job.as_ref().map(|j| j.finished).unwrap_or(true);
        if finished && matches!(code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
            self.install_job = None;
            if self.install_return == Screen::Tools {
                self.refresh_tool_status();
            }
            self.screen = self.install_return;
        }
    }
}

/// Edit an optional cylinder override on the read-options screen. `None` means
/// "format default": Right/typing sets a number, Left counts down to `None`, and
/// Backspace/Del clears back to the default. Capped at 84 (max realistic cyls).
fn adjust_track(value: &mut Option<u32>, code: KeyCode) {
    match code {
        KeyCode::Right => *value = Some(value.map_or(0, |n| (n + 1).min(84))),
        KeyCode::Left => *value = value.and_then(|n| n.checked_sub(1)),
        KeyCode::Backspace | KeyCode::Delete => *value = None,
        KeyCode::Char(c) if c.is_ascii_digit() => {
            let d = c as u32 - '0' as u32;
            *value = Some((value.unwrap_or(0) * 10 + d).min(84));
        }
        _ => {}
    }
}

fn is_typing(mods: KeyModifiers) -> bool {
    !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
}

/// Whether a gw format's media is hard-sectored (so `--hard-sectors` is pre-ticked).
fn is_hard_sectored(format: &str) -> bool {
    ["northstar", "micropolis"]
        .iter()
        .any(|p| format.starts_with(p))
}

/// Apply a key to a text field: printable chars insert at the cursor, and the
/// usual editing/navigation keys move or delete. Enter/Esc are handled by the
/// caller (they mean different things per screen).
fn edit_input(input: &mut TextInput, code: KeyCode, mods: KeyModifiers) {
    match code {
        KeyCode::Backspace => input.backspace(),
        KeyCode::Delete => input.delete(),
        KeyCode::Left => input.left(),
        KeyCode::Right => input.right(),
        KeyCode::Home => input.home(),
        KeyCode::End => input.end(),
        KeyCode::Char(c) if is_typing(mods) => input.insert(c),
        _ => {}
    }
}

fn move_list(state: &mut ListState, len: usize, delta: isize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or(0) as isize;
    let next = (current + delta).rem_euclid(len as isize) as usize;
    state.select(Some(next));
}

/// Wrap a plain `usize` cursor within `[0, len)`.
fn move_list_index(idx: &mut usize, len: usize, delta: isize) {
    if len == 0 {
        *idx = 0;
        return;
    }
    *idx = ((*idx as isize + delta).rem_euclid(len as isize)) as usize;
}

/// Formats offered by "Send to Gotek", in menu order. HFE first (the safe default).
pub const GOTEK_FORMATS: [GotekFormat; 3] = [
    GotekFormat::Hfe,
    GotekFormat::HfeV3,
    GotekFormat::CopyNative,
];

/// The output filename on the Gotek drive: keep the name for copy-as-is, else swap
/// the extension for `.hfe`.
pub fn gotek_out_name(source_name: &str, format: GotekFormat) -> String {
    match format.extension() {
        Some(ext) => {
            let stem = Path::new(source_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(source_name);
            format!("{stem}.{ext}")
        }
        None => source_name.to_string(),
    }
}

fn file_name(path: &PathBuf) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("image")
        .to_string()
}

fn item_file_name(item: &MediaItem) -> String {
    Path::new(&item.path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(item.path.as_str())
        .to_string()
}

/// True if `dir` has no entries at all (a freshly created organising folder).
/// Unreadable dirs count as empty so a permissions hiccup never wrongly hides a
/// folder the user just made.
fn dir_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true)
}

/// Turn an in-image file name into a safe single host-filename component. TRS-80
/// names are `NAME/EXT`; the `/` maps to `.` (as it does on real TRS-80 hosts),
/// and any stray separator is neutralised so the name can't escape its folder.
fn safe_host_name(name: &str) -> String {
    name.replace('/', ".").replace('\\', "_")
}

/// Whether `name` is a non-representative member of a KryoFlux stream capture and
/// should be hidden from the library view. KryoFlux writes one `.raw` file per
/// track/side, named `<base><TT>.<S>.raw` (zero-padded 2+ digit track, 1-digit
/// side). `hxcfe` loads the whole disk from any one of them, so we keep only the
/// track-0 side-0 file and hide every other track/side. Standalone `.raw`
/// captures (no `<TT>.<S>` suffix) are never hidden.
fn is_hidden_flux_track(name: &str) -> bool {
    let lower = name.to_lowercase();
    let Some(stem) = lower.strip_suffix(".raw") else {
        return false;
    };
    let Some((head, side)) = stem.rsplit_once('.') else {
        return false;
    };
    if side.len() != 1 || !side.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let mut track_digits: Vec<char> =
        head.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
    // KryoFlux tracks are always zero-padded to ≥2 digits; a single trailing
    // digit is more likely part of the disk's name than a track number.
    if track_digits.len() < 2 {
        return false;
    }
    track_digits.reverse();
    let track: u32 = track_digits.iter().collect::<String>().parse().unwrap_or(0);
    let side: u32 = side.parse().unwrap_or(0);
    track != 0 || side != 0
}

/// A path alongside `base` with its extension replaced by `ext`, made unique by
/// appending ` (2)`, ` (3)`, … if something is already there.
fn unique_sibling(base: &Path, ext: &str) -> PathBuf {
    let candidate = base.with_extension(ext);
    if !candidate.exists() {
        return candidate;
    }
    let dir = base.parent().unwrap_or_else(|| Path::new("."));
    let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
    for n in 2..1000 {
        let candidate = dir.join(format!("{stem} ({n}).{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    base.with_extension(ext)
}

#[cfg(test)]
mod flux_tracks {
    use super::is_hidden_flux_track;

    #[test]
    fn hides_all_but_track0_side0_of_a_kryoflux_set() {
        // The representative file stays visible.
        assert!(!is_hidden_flux_track("TRS-SPOOK00.0.raw"));
        // Other tracks / the second side are hidden.
        assert!(is_hidden_flux_track("TRS-SPOOK01.0.raw"));
        assert!(is_hidden_flux_track("TRS-SPOOK19.0.raw"));
        assert!(is_hidden_flux_track("TRS-SPOOK00.1.raw"));
        // Standalone captures and non-raw files are never hidden.
        assert!(!is_hidden_flux_track("disk.raw"));
        assert!(!is_hidden_flux_track("capture.0.raw"));
        assert!(!is_hidden_flux_track("game2.0.raw"));
        assert!(!is_hidden_flux_track("Burntime-Disk4.adf"));
    }
}

#[cfg(test)]
mod format_labels {
    use super::*;

    #[test]
    fn label_editor_prefills_and_cancels_without_saving() {
        let mut app = App::new(Core::init().unwrap());
        app.formats = vec!["ibm.1440".to_string()];
        app.screen = Screen::FormatPicker;
        app.reset_format_selection();

        // Effective label falls back to the generated description.
        assert_eq!(app.format_label("ibm.1440"), gwm_core::formats::describe_format("ibm.1440"));

        // Ctrl+E opens the editor pre-filled with that label.
        app.test_key(KeyCode::Char('e'), KeyModifiers::CONTROL);
        let (fmt, input) = app.format_editing.as_ref().expect("editor should be open");
        assert_eq!(fmt, "ibm.1440");
        assert_eq!(input.text(), gwm_core::formats::describe_format("ibm.1440"));

        // Typing edits the buffer; Esc cancels without persisting an override.
        app.test_key(KeyCode::Char('!'), KeyModifiers::NONE);
        app.test_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.format_editing.is_none());
        assert!(!app.core.settings.format_labels.contains_key("ibm.1440"));
    }
}

#[cfg(test)]
mod archive_live {
    //! On-demand checks against the live archive.org API (network-gated, so
    //! `#[ignore]`d): `cargo test -p gwm-tui -- --ignored archive`.
    use super::*;

    /// Pump the in-flight [`NetJob`] to completion (up to ~30s) and route it.
    fn pump_net(app: &mut App) {
        for _ in 0..300 {
            if app.net_job.as_mut().map(NetJob::pump).unwrap_or(false) {
                app.finalize_net_fetch();
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("archive.org fetch timed out");
    }

    #[test]
    #[ignore = "hits the live archive.org API"]
    fn live_search_then_files_flow() {
        let mut app = App::new(Core::init().unwrap());

        // Type a query and search, through the real key handlers + worker thread.
        app.screen = Screen::ArchiveSearch;
        for c in "amiga workbench".chars() {
            app.test_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        app.test_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::ArchiveFetching);
        pump_net(&mut app);
        assert_eq!(app.screen, Screen::ArchiveResults);
        assert!(!app.archive_hits.is_empty(), "search returned no hits");

        // Open a deterministic item known to hold .adz images, and list its files.
        app.archive_hits = vec![SearchHit {
            identifier: "commodore-amiga-floppy-disk-images".into(),
            title: "Commodore Amiga Floppy Disk Images".into(),
            downloads: 0,
            mediatype: "software".into(),
        }];
        app.archive_hits_state.select(Some(0));
        app.test_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::ArchiveFetching);
        pump_net(&mut app);
        assert_eq!(app.screen, Screen::ArchiveFiles);
        assert!(!app.archive_files.is_empty(), "no importable image files found");
        assert!(app.archive_files.iter().any(|f| f.is_gzipped()));
    }
}
