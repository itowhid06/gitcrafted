//! One open repository: its state, background git work and the view
//! (branches on the left, commit graph or diff in the middle, commit detail / changes on the right).

mod branch_filter;
mod conflict;
mod detail;
mod dialogs;
mod diff;
mod file_view;
mod graph_view;
pub(crate) mod highlight;
mod bisect;
mod branch_ops;
mod ext;
mod external;
mod issues;
mod lfs;
mod maintenance;
mod patch;
mod reflog;
mod rebase;
mod rewrite;
mod selection;
mod sidebar;
#[cfg(test)]
mod test_support;
mod sync;
mod submodules;
pub(crate) mod widgets;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use egui::{Key, Ui};

use crate::forge::{self, Forge, PullRequest};
use crate::git::{
    self, Commit, CommitDetail, Identity, InProgress, Refs, Remote, Stash, StatusFile, WorkStatus,
    Worktree,
};
use crate::graph::{self, Graph};
use crate::icons;
use crate::settings::Settings;
use crate::terminal::{ShellSpec, Terminal};
use crate::theme::Tokens;
pub(crate) use branch_filter::BranchFilter;

pub use dialogs::Dialog;
pub use widgets::primary_button;

/// Hash of the pseudo-commit that stands for uncommitted changes at the top of the graph.
pub(crate) const WIP_HASH: &str = "\u{0}wip";
/// History deeper than this isn't loaded by scrolling or searching.
const MAX_HISTORY: usize = 100_000;
/// An operation that takes longer than this gets a notification when it ends.
const LONG_OP: Duration = Duration::from_secs(4);

/// Results coming back from background git calls.
enum Msg {
    Loaded(u64, Result<Box<Loaded>, String>),
    More(Result<Vec<Commit>, String>),
    Status(u64, Result<WorkStatus, String>),
    Detail(u64, String, Result<CommitDetail, String>),
    /// The answer to the diff request numbered so (see [`DiffView::next_request`]).
    Diff(u64, Result<diff::DiffData, String>),
    FileHistory(u64, String, Result<Vec<git::FileCommit>, String>),
    Blame(u64, String, Option<String>, Result<file_view::BlameData, String>),
    RebaseCommits {
        seq: u64,
        onto: String,
        label: String,
        result: Result<Vec<git::RebaseCommit>, String>,
    },
    Search(u64, String, Result<Vec<String>, String>),
    /// A comparison's files, for the request numbered so.
    Compare(u64, Result<CompareData, String>),
    Prs(u64, Result<Vec<PullRequest>, String>),
    /// The commits to replay for a quick edit of `hash` (rewording, dropping or moving it).
    QuickEditCommits {
        seq: u64,
        hash: String,
        edit: QuickEdit,
        result: Result<Vec<git::RebaseCommit>, String>,
    },
    OpDone {
        label: String,
        result: Result<String, String>,
        refresh: Refresh,
        kind: OpKind,
        quiet: bool,
        failed_step: Option<usize>,
    },
    WatcherReady(notify::RecommendedWatcher, Receiver<bool>),
}

pub(super) enum RepoPicker {
    WorktreeDir,
    ApplyPatch,
    SavePatch(patch::PatchSource),
}

struct Loaded {
    refs: Refs,
    history: Vec<Commit>,
    work: WorkStatus,
    worktrees: Vec<Worktree>,
    submodules: Vec<submodules::Submodule>,
    stashes: Vec<Stash>,
    remotes: Vec<Remote>,
    identity: Identity,
    head_message: Option<String>,
    bisect: Option<bisect::Info>,
    issue_rules: Vec<issues::Rule>,
    /// How many commits were asked for (more exist when that many came back).
    requested: usize,
}

/// What to re-read after an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refresh {
    /// Only the working tree (staging changes nothing else).
    Status,
    All,
}

/// How an operation can be undone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Undo {
    None,
    /// The operation moved HEAD (commit, merge, reset…): undo moves it back with
    /// `git reset <mode>`, redo forward again. `--soft` keeps the changes staged.
    HeadMove {
        mode: &'static str,
    },
    /// Explicit commands each way (branch create/delete/rename, checkout, tags, stash).
    Custom {
        undo: Vec<Vec<String>>,
        redo: Vec<Vec<String>>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct UndoEntry {
    label: String,
    undo: Vec<Vec<String>>,
    redo: Vec<Vec<String>>,
}

/// An operation as `run_ops_ex` got it, kept to retry it with credentials.
struct PendingOp {
    label: String,
    ops: Vec<Vec<String>>,
    refresh: Refresh,
    kind: OpKind,
    quiet: bool,
    remote_url: String,
    autostash: bool,
}

/// A PR/MR creation waiting on HTTPS credentials for its initial `git push`.
struct PendingPr {
    branch: String,
    base: String,
    title: String,
    body: String,
    draft: bool,
    remote_url: String,
}

/// Uncommitted changes stashed around an operation and applied again after it.
const AUTOSTASH: &str = "gitcrafted: autostash";
/// Starts the error of an operation whose stashed changes didn't apply cleanly after it.
const AUTOSTASH_CONFLICT: &str = "Your changes conflict with the new files";

/// Choices shown in place of the toolbar for a moment, when what was asked needs one.
pub(crate) struct Prompt {
    text: String,
    /// (label, tooltip, what it does, whether it throws something away)
    options: Vec<(String, String, Action, bool)>,
}

#[derive(Clone, Debug)]
enum OpKind {
    Normal(Undo, String),
    Undo(UndoEntry),
    Redo(UndoEntry),
}

/// Which diff the central area shows.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DiffKey {
    Commit {
        hash: String,
        path: String,
    },
    Work(StatusFile),
    /// A file from one commit to another, or to the working tree (`to: None`).
    Range {
        from: String,
        to: Option<String>,
        path: String,
    },
}

pub(crate) struct DiffView {
    key: DiffKey,
    data: Option<Result<diff::DiffData, String>>,
    /// Image URIs previously loaded on this view that must be evicted from egui's loader cache.
    pub(super) stale_uris: Vec<String>,
    /// Changed lines clicked, to stage, unstage or discard only those.
    chosen: std::collections::BTreeSet<usize>,
    /// The last line clicked, where a Shift+click range starts.
    anchor: Option<usize>,
    /// Text selected by dragging over the lines, to copy.
    text_sel: Option<diff::TextSel>,
    /// The latest read of this diff: only its answer is shown, so a slower answer to an older
    /// read (before the whitespace or whole-file setting changed…) can't replace it.
    request: u64,
    /// How far down it was scrolled last frame, to jump to the change before or after.
    scroll_y: f32,
}

/// Numbers the diff reads of every tab, so no two diffs ever wait for the same number.
static DIFF_REQUESTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl DiffView {
    fn new(key: DiffKey) -> Self {
        Self {
            key,
            data: None,
            stale_uris: Vec::new(),
            chosen: Default::default(),
            anchor: None,
            text_sel: None,
            request: 0,
            scroll_y: 0.0,
        }
    }

    /// Numbers a new read of this diff; from now on only its answer is shown.
    fn next_request(&mut self) -> u64 {
        self.request = DIFF_REQUESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        self.request
    }

    fn forget_images(&mut self, ctx: &egui::Context) {
        if let Some(Ok(data)) = &self.data
            && let Some(img) = &data.image
        {
            for uri in img.uris() {
                ctx.forget_image(&uri);
            }
        }
        for uri in self.stale_uris.drain(..) {
            ctx.forget_image(&uri);
        }
    }

    /// Shows the answer to the read numbered `request`, unless a newer read was made since.
    fn arrived(&mut self, request: u64, result: Result<diff::DiffData, String>) {
        if request != self.request {
            return;
        }
        if let Some(Ok(prev)) = &self.data
            && let Some(img) = &prev.image
        {
            self.stale_uris.extend(img.uris());
        }
        self.data = Some(result);
        // The lines may have moved.
        self.chosen.clear();
        self.anchor = None;
        self.text_sel = None;
    }
}

/// A change to one commit of the checked-out branch, made from its menu (or its message in the
/// right pane) without the interactive rebase screen.
pub(crate) enum QuickEdit {
    Reword(String),
    Drop,
    /// Swapped with the commit after it.
    MoveUp,
    /// Swapped with the commit before it.
    MoveDown,
}

/// A commit's message being edited in the right pane (double-click on it).
pub(crate) struct Reword {
    pub hash: String,
    pub subject: String,
    pub body: String,
    /// Put the cursor in the summary on the next frame.
    pub focus: bool,
}

/// Two points of history side by side in the right pane: a branch against another, a commit
/// against the working tree…
pub(crate) struct Compare {
    base: String,
    base_label: String,
    /// `None`: the working tree.
    target: Option<String>,
    target_label: String,
    /// Only what the target changed since the two diverged (from their merge base), like a pull
    /// request shows it.
    since_fork: bool,
    data: Option<Result<CompareData, String>>,
    /// Numbers the requests, so an older answer doesn't replace a newer one.
    request: u64,
}

pub(crate) struct CompareData {
    /// Where the diff starts: the base's commit, or the merge base.
    from: String,
    /// The target's commit (`None`: the working tree).
    to: Option<String>,
    files: Vec<git::ChangedFile>,
    /// Commits only the base has, and only the target (HEAD for the working tree) has.
    apart: (u32, u32),
}

impl Compare {
    /// How the comparison calls one of its two ends.
    fn name_of(&self, rev: &str) -> Option<String> {
        let data = self.data.as_ref()?.as_ref().ok()?;
        if data.to.as_deref() == Some(rev) {
            Some(self.target_label.clone())
        } else if data.from == rev && self.since_fork {
            Some(format!("merge base {}", short(rev)))
        } else if data.from == rev {
            Some(self.base_label.clone())
        } else {
            None
        }
    }
}

#[derive(Default)]
struct Search {
    open: bool,
    focus: bool,
    /// Search what commits added or removed in files (`git log -S`), not their messages.
    code: bool,
    query: String,
    /// The query the results are for.
    searched: String,
    results: Vec<String>,
    matches: HashSet<String>,
    current: usize,
    running: bool,
    edited: Option<Instant>,
}

/// Everything the views ask for; applied after drawing so views only borrow the state.
pub(crate) enum Action {
    Select(usize),
    SelectHash(String),
    SelectStash(usize),
    OpenDiff(DiffKey),
    CloseDiff,
    Ops {
        label: String,
        ops: Vec<Vec<String>>,
        refresh: Refresh,
    },
    UndoableOps {
        label: String,
        ops: Vec<Vec<String>>,
        refresh: Refresh,
        undo: Undo,
    },
    Checkout(String),
    /// Checks out a local branch behind its upstream, then fast-forwards it.
    /// Hides, shows or solos a branch in the graph.
    BranchFilter(branch_filter::Change),
    /// Moves local `branch` to `target` (further on): checks it out if needed and
    /// fast-forwards it, the uncommitted changes stashed around; when they have diverged, the
    /// toolbar offers what can be done instead.
    MoveBranch {
        branch: String,
        target: String,
    },
    CheckoutRemote(String),
    Dialog(Dialog),
    Commit,
    Fetch,
    Pull,
    Push,
    Reload,
    LoadMore,
    Undo,
    Redo,
    StashPush(String),
    /// Open a folder (a worktree) in its own tab.
    OpenTab(PathBuf),
    ToggleTerminal,
    NewTerminal {
        shell: Option<ShellSpec>,
        cwd: Option<PathBuf>,
    },
    CloseTerminal(usize),
    SelectTerminal(usize),
    ToggleSearch,
    SearchStep(i32),
    CreatePr {
        branch: String,
        base: String,
        title: String,
        body: String,
        draft: bool,
    },
    OpenUrl(String),
    /// Open a path (relative to the repository) with the system's default app.
    OpenPath(PathBuf),
    OpenInEditor(PathBuf),
    /// A file in the editor, at a line (1-based).
    OpenInEditorAt(PathBuf, u32),
    SetIdentity(String, String),
    /// How diffs show: side by side, without whitespace changes, the whole file.
    SetDiffOptions {
        split: bool,
        ignore_ws: bool,
        whole_file: bool,
    },
    SaveConflict,
    /// A file's history or blame in the central pane (`rev`: the commit to blame at).
    FileView {
        path: String,
        rev: Option<String>,
        tab: file_view::FileTab,
    },
    CloseFileView,
    /// Open the interactive rebase editor for the commits after `onto`.
    /// Gives a commit of the checked-out branch a new message: amending the latest, rebasing
    /// for an older one.
    Reword {
        hash: String,
        message: String,
    },
    /// Select a commit and open its message editor.
    EditMessage(String),
    /// Drops a commit, or swaps it with its neighbour, by a rebase from below it.
    QuickEdit(String, QuickEdit),
    /// Save a patch, or select files for one.
    Patch(patch::PatchAction),
    /// The extra features' actions (see `ext.rs`).
    Ext(ext::ExtAction),
    /// A fetch with the given arguments (`git fetch …`), from the Fetch dialog.
    FetchWith(Vec<String>),
    InteractiveRebase {
        onto: String,
        label: String,
    },
    RunRebase,
    CloseRebase,
    /// Remember these HTTPS credentials and run the operation that needed them again.
    SignIn(git::Credentials),
    /// Add `pattern` to `.gitignore`; stop tracking `path` if git tracks it.
    Ignore {
        pattern: String,
        path: String,
    },
    /// Stage, unstage or discard these changed lines of the open diff.
    PickLines {
        pick: diff::Pick,
        lines: std::collections::BTreeSet<usize>,
    },
    /// Compare `base` with `target` (the working tree when `None`) in the right pane.
    Compare {
        base: String,
        base_label: String,
        target: Option<String>,
        target_label: String,
        since_fork: bool,
    },
    /// Show only the target's changes since the two diverged, or every difference.
    CompareSinceFork(bool),
    SwapCompare,
    CloseCompare,
    /// Show the working-tree changes and commit box (even when clean).
    ShowLocalChanges,
    /// Clear the toolbar status/error message.
    DismissStatus,
    /// Search commit messages, or what commits changed in files.
    SearchCode(bool),
}

pub struct RepoTab {
    pub path: PathBuf,
    pub name: String,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,

    loading: bool,
    reload_queued: bool,
    load_seq: u64,
    status_seq: u64,
    detail_seq: u64,
    file_seq: u64,
    search_seq: u64,
    prs_seq: u64,
    rebase_seq: u64,
    load_error: Option<String>,
    refs: Refs,
    /// Commits as git listed them.
    history: Vec<Commit>,
    has_more: bool,
    loading_more: bool,
    work: WorkStatus,
    /// Rows of the graph: the WIP pseudo-commit (when the tree is dirty) followed by `history`.
    commits: Vec<Commit>,
    graph: Graph,
    index: HashMap<String, usize>,
    worktrees: Vec<Worktree>,
    submodules: Vec<submodules::Submodule>,
    stashes: Vec<Stash>,
    remotes: Vec<Remote>,
    identity: Identity,
    head_message: Option<String>,
    pub(super) bisect: Option<bisect::Info>,
    pub(super) stored_issue_rules: Vec<issues::Rule>,
    forge: Option<Forge>,
    prs: Vec<PullRequest>,
    pr_error: Option<String>,
    prs_loaded: Option<Instant>,

    selected: Option<usize>,
    /// A stash shown in the detail pane instead of a graph row.
    stash_view: Option<usize>,
    scroll_to: Option<usize>,
    /// The commit the detail pane is about (a graph row or a stash).
    detail_hash: Option<String>,
    detail: Option<CommitDetail>,
    detail_error: Option<String>,
    diff: Option<DiffView>,
    compare: Option<Compare>,
    compare_requests: u64,
    file_view: Option<file_view::FileView>,
    rebase: Option<rebase::RebaseEditor>,
    /// The selected commit's message, being edited in the right pane.
    reword: Option<Reword>,
    /// HTTPS credentials typed in this session, keyed by remote host origin.
    credentials: HashMap<String, git::Credentials>,
    /// The last user-initiated operation started, to run again after signing in.
    last_op: Option<PendingOp>,
    pending_pr: Option<PendingPr>,
    /// Whether an egui widget held keyboard focus at the end of the previous frame (for Esc).
    was_typing: bool,
    /// Structured checkout target while a checkout is running (for blocked-checkout recovery).
    pending_checkout: Option<branch_ops::CheckoutTarget>,
    /// The forge has been asked for avatars.
    avatars_hinted: bool,
    diff_ignore_ws: bool,
    conflict: Option<conflict::ConflictEditor>,

    commit_summary: String,
    commit_body: String,
    amend: bool,
    /// Commit with `--no-verify`: the pre-commit and commit-msg hooks don't run.
    no_verify: bool,
    dialog: Option<Dialog>,
    search: Search,
    /// A commit to select once enough history is loaded (search results, links).
    pending_goto: Option<String>,
    /// A commit whose message editor opens once its details are loaded.
    pending_edit: Option<String>,
    /// Files Ctrl+clicked in the Changes lists, to save as a patch.
    patch_sel: Vec<StatusFile>,
    /// Where a Shift+click in the Changes lists starts from.
    sel_anchor: Option<StatusFile>,
    /// The Changes list last clicked (`true`: staged), which Ctrl+A selects when the pointer isn't
    /// over one.
    sel_staged: bool,
    /// Where a Commit & Push is: waiting for the commit, or pushing after it.
    push_follow: ext::PushFollow,

    undo_stack: Vec<UndoEntry>,
    redo_stack: Vec<UndoEntry>,

    busy_op: Option<String>,
    op_started: Option<Instant>,
    status: Option<(String, bool)>,
    branch_filter: String,
    /// Jump to the changes row on the next rebuild (after an operation left conflicts).
    focus_wip: bool,
    /// Keep the Local changes / commit panel open even when the working tree is clean.
    show_local_changes: bool,
    /// A question in place of the toolbar (see [`Prompt`]).
    prompt: Option<Prompt>,
    /// Branches hidden from the graph or soloed in it.
    pub(crate) graph_filter: BranchFilter,
    last_fetch: Instant,

    terminals: Vec<Terminal>,
    active_terminal: usize,
    terminal_open: bool,
    terminal_error: Option<String>,

    watcher: Option<notify::RecommendedWatcher>,
    fs_rx: Option<Receiver<bool>>,
    /// A change on disk not yet reloaded: (inside .git, when).
    fs_pending: Option<(bool, Instant)>,
    file_picker: Option<(RepoPicker, crate::file_dialog::PendingDialog)>,

    /// Actions queued from outside (the command palette).
    queued: Vec<Action>,
    /// A folder the app should open in a new tab (taken by `App`).
    pub open_request: Option<PathBuf>,
    /// The app has a dialog of its own open (palette, settings): leave the keys to it.
    pub keys_blocked: bool,
    /// A long operation that just ended: (message, failed), for a notification (taken by `App`).
    pub finished: Option<(String, bool)>,
}

impl RepoTab {
    /// A tab whose graph starts with `filter`'s branches hidden or soloed.
    pub fn with_filter(path: PathBuf, filter: BranchFilter, ctx: &egui::Context) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let (tx, rx) = channel();
        let mut tab = Self {
            path,
            name,
            tx,
            rx,
            loading: false,
            reload_queued: false,
            load_seq: 0,
            status_seq: 0,
            detail_seq: 0,
            file_seq: 0,
            search_seq: 0,
            prs_seq: 0,
            rebase_seq: 0,
            load_error: None,
            refs: Refs::default(),
            history: Vec::new(),
            has_more: false,
            loading_more: false,
            work: WorkStatus::default(),
            commits: Vec::new(),
            graph: Graph {
                rows: Vec::new(),
                width: 0,
            },
            index: HashMap::new(),
            worktrees: Vec::new(),
            submodules: Vec::new(),
            stashes: Vec::new(),
            remotes: Vec::new(),
            identity: Identity::default(),
            head_message: None,
            bisect: None,
            stored_issue_rules: Vec::new(),
            forge: None,
            prs: Vec::new(),
            pr_error: None,
            prs_loaded: None,
            selected: None,
            stash_view: None,
            scroll_to: None,
            detail_hash: None,
            detail: None,
            detail_error: None,
            diff: None,
            compare: None,
            compare_requests: 0,
            file_view: None,
            rebase: None,
            reword: None,
            credentials: HashMap::new(),
            last_op: None,
            pending_pr: None,
            was_typing: false,
            pending_checkout: None,
            avatars_hinted: false,
            diff_ignore_ws: false,
            conflict: None,
            commit_summary: String::new(),
            commit_body: String::new(),
            amend: false,
            no_verify: false,
            dialog: None,
            search: Search::default(),
            pending_goto: None,
            pending_edit: None,
            patch_sel: Vec::new(),
            sel_anchor: None,
            sel_staged: false,
            push_follow: ext::PushFollow::Idle,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            busy_op: None,
            op_started: None,
            status: None,
            branch_filter: String::new(),
            focus_wip: false,
            show_local_changes: false,
            prompt: None,
            graph_filter: BranchFilter::default(),
            last_fetch: Instant::now(),
            terminals: Vec::new(),
            active_terminal: 0,
            terminal_open: false,
            terminal_error: None,
            watcher: None,
            fs_rx: None,
            fs_pending: None,
            file_picker: None,
            queued: Vec::new(),
            open_request: None,
            keys_blocked: false,
            finished: None,
        };
        tab.watch(ctx);
        tab.graph_filter = filter;
        tab.reload(ctx);
        tab
    }

    pub fn is_busy(&self) -> bool {
        self.loading || self.busy_op.is_some()
    }

    pub fn has_dialog(&self) -> bool {
        self.dialog.is_some() || self.file_picker.is_some()
    }

    /// Queues an action from outside the tab (the command palette).
    pub(crate) fn queue(&mut self, action: Action) {
        self.queued.push(action);
    }

    /// Re-reads refs, history, working tree, worktrees, stashes and remotes in the background.
    pub fn reload(&mut self, ctx: &egui::Context) {
        if self.loading {
            self.reload_queued = true;
            return;
        }
        self.loading = true;
        self.reload_queued = false;
        self.load_seq = self.load_seq.wrapping_add(1);
        self.status_seq = self.status_seq.wrapping_add(1);
        let seq = self.load_seq;
        let path = self.path.clone();
        // Keep the pages already loaded.
        let count = self.history.len().clamp(git::LOG_PAGE, MAX_HISTORY);
        let revs = self.graph_filter.revs();
        spawn(&self.tx, ctx, move || {
            Msg::Loaded(
                seq,
                (|| {
                    let refs = git::refs(&path)?;
                    let history = git::log(&path, &revs, 0, count)?;
                    let work = git::status(&path)?;
                    // Very old gits have no `worktree list`; the section just stays empty.
                    let worktrees = git::worktrees(&path).unwrap_or_default();
                    let submodules = submodules::list(&path);
                    let stashes = git::stashes(&path).unwrap_or_default();
                    let remotes = git::remotes(&path).unwrap_or_default();
                    let identity = git::identity(&path);
                    let head_message = (!refs.head.is_empty())
                        .then(|| git::head_message(&path).ok())
                        .flatten();
                    let bisect = bisect::read_state(&path, &refs.head);
                    let issue_rules = issues::read_rules(&path);
                    Ok(Box::new(Loaded {
                        refs,
                        history,
                        work,
                        worktrees,
                        submodules,
                        stashes,
                        remotes,
                        identity,
                        head_message,
                        bisect,
                        issue_rules,
                        requested: count,
                    }))
                })(),
            )
        });
    }

    fn reload_status(&mut self, ctx: &egui::Context) {
        if self.loading {
            self.reload_queued = true;
            return;
        }
        self.status_seq = self.status_seq.wrapping_add(1);
        let seq = self.status_seq;
        let path = self.path.clone();
        spawn(&self.tx, ctx, move || Msg::Status(seq, git::status(&path)));
    }

    fn load_more(&mut self, ctx: &egui::Context) {
        if self.loading_more || !self.has_more || self.history.len() >= MAX_HISTORY {
            return;
        }
        self.loading_more = true;
        let path = self.path.clone();
        let skip = self.history.len();
        let revs = self.graph_filter.revs();
        spawn(&self.tx, ctx, move || {
            Msg::More(git::log(&path, &revs, skip, git::LOG_PAGE))
        });
    }

    fn load_prs(&mut self, ctx: &egui::Context) {
        let Some(forge) = self.forge else { return };
        self.prs_loaded = Some(Instant::now());
        self.prs_seq = self.prs_seq.wrapping_add(1);
        let seq = self.prs_seq;
        let path = self.path.clone();
        spawn(&self.tx, ctx, move || Msg::Prs(seq, forge::list(&path, forge)));
    }

    /// Watches the folder (and resolved git dir for linked worktrees) in the background so
    /// external changes show up without blocking tab opening.
    fn watch(&mut self, ctx: &egui::Context) {
        use notify::Watcher;
        let path = self.path.clone();
        let msg_tx = self.tx.clone();
        let ctx_watch = ctx.clone();
        std::thread::spawn(move || {
            let resolve_dir = |arg: &str| -> Option<PathBuf> {
                let raw = git::run(Some(&path), &["rev-parse", arg]).ok()?;
                let p = PathBuf::from(raw.trim_end_matches(['\r', '\n']));
                let full = if p.is_absolute() { p } else { path.join(p) };
                Some(std::fs::canonicalize(&full).unwrap_or(full))
            };
            let git_dir = resolve_dir("--git-dir");
            let common_dir = resolve_dir("--git-common-dir");
            let work_root = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            let norm_dir = |p: &Path| -> String {
                let s = p.to_string_lossy().replace('\\', "/");
                format!("{}/", s.trim_end_matches('/'))
            };
            let git_dir_norm = git_dir.as_deref().map(norm_dir);
            let common_dir_norm = common_dir.as_deref().map(norm_dir);

            let (tx, rx) = channel();
            let ctx_ev = ctx_watch.clone();
            let handler = move |res: notify::Result<notify::Event>| {
                let Ok(event) = res else { return };
                if matches!(event.kind, notify::EventKind::Access(_)) {
                    return;
                }
                for p in &event.paths {
                    let s = p.to_string_lossy().replace('\\', "/");
                    let inside = s
                        .split_once("/.git/")
                        .map(|(_, rest)| rest)
                        .or_else(|| {
                            git_dir_norm
                                .as_deref()
                                .and_then(|g| s.strip_prefix(g))
                                .map(|rest| rest.trim_start_matches('/'))
                        })
                        .or_else(|| {
                            common_dir_norm
                                .as_deref()
                                .and_then(|g| s.strip_prefix(g))
                                .map(|rest| rest.trim_start_matches('/'))
                        });
                    match inside {
                        // Refs, HEAD, the index and merge/rebase state matter; locks/objects/logs don't.
                        Some(inside) => {
                            if inside.ends_with(".lock") {
                                continue;
                            }
                            let relevant = inside.starts_with("refs")
                                || inside == "HEAD"
                                || inside == "index"
                                || inside == "packed-refs"
                                || inside.ends_with("_HEAD")
                                || inside.starts_with("rebase-")
                                || inside.starts_with("MERGE_")
                                || inside.starts_with("BISECT_");
                            if relevant {
                                let _ = tx.send(true);
                                ctx_ev.request_repaint_after(Duration::from_millis(500));
                            }
                        }
                        None => {
                            let _ = tx.send(false);
                            ctx_ev.request_repaint_after(Duration::from_millis(500));
                        }
                    }
                }
            };
            let Ok(mut watcher) = notify::recommended_watcher(handler) else {
                return;
            };
            if watcher
                .watch(&path, notify::RecursiveMode::Recursive)
                .is_err()
            {
                return;
            }
            for extra in [git_dir.as_ref(), common_dir.as_ref()].into_iter().flatten() {
                if !extra.starts_with(&work_root) && extra.is_dir() {
                    let _ = watcher.watch(extra, notify::RecursiveMode::Recursive);
                }
            }
            let _ = msg_tx.send(Msg::WatcherReady(watcher, rx));
            ctx_watch.request_repaint();
        });
    }

    pub(super) fn showing_local_changes(&self) -> bool {
        self.compare.is_none()
            && (self.show_local_changes || self.selected.is_some_and(|r| self.is_wip(r)))
    }

    fn is_wip(&self, row: usize) -> bool {
        self.commits.get(row).is_some_and(|c| c.hash == WIP_HASH)
    }

    /// The stash a graph row stands for, if it's one.
    fn stash_at(&self, row: usize) -> Option<usize> {
        let hash = &self.commits.get(row)?.hash;
        self.stashes.iter().position(|s| &s.hash == hash)
    }

    fn selected_hash(&self) -> Option<String> {
        self.selected
            .and_then(|r| self.commits.get(r))
            .map(|c| c.hash.clone())
    }

    /// Rebuilds the graph rows from `history` and `work`, keeping the selection.
    fn rebuild(&mut self, ctx: &egui::Context) {
        let previous = self.selected_hash();
        self.commits.clear();
        if !self.work.is_clean() {
            self.commits.push(Commit {
                hash: WIP_HASH.to_owned(),
                parents: if self.refs.head.is_empty() {
                    Vec::new()
                } else {
                    vec![self.refs.head.clone()]
                },
                time: git::now(),
                ..Commit::default()
            });
        }
        // Each stash sits right above the commit it was made on, as a commit on no branch.
        for commit in &self.history {
            for stash in self.stashes.iter().filter(|s| s.base == commit.hash) {
                self.commits.push(Commit {
                    hash: stash.hash.clone(),
                    parents: vec![stash.base.clone()],
                    author: stash.author.clone(),
                    email: stash.email.clone(),
                    time: stash.time,
                    date: stash.date.clone(),
                    summary: stash.message.clone(),
                    ..Commit::default()
                });
            }
            self.commits.push(commit.clone());
        }
        self.graph = graph::layout(&self.commits);
        self.index = self
            .commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.hash.clone(), i))
            .collect();

        // Keep the selection; otherwise start on the changes, or HEAD.
        let mut keep = previous.as_ref().and_then(|h| self.index.get(h).copied());
        if self.show_local_changes && self.index.contains_key(WIP_HASH) {
            self.show_local_changes = false;
            self.focus_wip = true;
        }
        if std::mem::take(&mut self.focus_wip) && self.index.contains_key(WIP_HASH) {
            keep = None;
            self.stash_view = None;
        }
        if self.stash_view.is_none() {
            let target = keep
                .or_else(|| self.index.get(WIP_HASH).copied())
                .or_else(|| self.index.get(&self.refs.head).copied());
            match target {
                Some(row) if keep == Some(row) => self.selected = Some(row),
                Some(row) => {
                    self.selected = None;
                    self.select(row, ctx);
                    self.scroll_to = Some(row);
                }
                None => {
                    self.selected = None;
                    self.detail = None;
                }
            }
        } else {
            self.selected = self
                .stash_view
                .and_then(|i| self.index.get(&self.stashes.get(i)?.hash).copied());
        }

        // A merge or cherry-pick leaves a message to commit with.
        if let Some(msg) = &self.work.prepared_message
            && self.commit_summary.is_empty()
            && self.commit_body.is_empty()
        {
            let (summary, body) = msg.split_once('\n').unwrap_or((msg.as_str(), ""));
            self.commit_summary = summary.trim().to_owned();
            self.commit_body = body.trim().to_owned();
        }

        // The diff of a file that's no longer listed (staged, committed…) is stale.
        // Preserve an active ConflictEditor while the file remains conflicted so background reloads
        // do not discard the user's edits or button choices.
        if let Some(DiffView {
            key: DiffKey::Work(file),
            ..
        }) = &self.diff
        {
            let still = self
                .work
                .staged
                .iter()
                .chain(&self.work.unstaged)
                .chain(&self.work.conflicted)
                .any(|f| f == file);
            let keeping_conflict = still
                && file.area == git::Area::Conflicted
                && self
                    .conflict
                    .as_ref()
                    .is_some_and(|c| c.path == file.path);
            let key = DiffKey::Work(file.clone());
            if keeping_conflict {
                // Keep the open conflict editor untouched.
            } else if still && file.area != git::Area::Conflicted {
                // Shown as it was until the new diff is read (no flicker after staging lines).
                self.conflict = None;
                self.refresh_diff(ctx);
            } else {
                self.clear_diff(ctx);
                if still {
                    self.open_diff(key, ctx);
                }
            }
        }

        if let Some(hash) = self.pending_goto.clone() {
            self.goto(&hash, ctx);
        }
    }

    fn clear_diff(&mut self, ctx: &egui::Context) {
        if let Some(mut view) = self.diff.take() {
            view.forget_images(ctx);
        }
        self.conflict = None;
    }

    fn select(&mut self, row: usize, ctx: &egui::Context) {
        let Some(commit) = self.commits.get(row) else {
            return;
        };
        let hash = commit.hash.clone();
        let stash = self.stash_at(row);
        self.file_view = None;
        self.compare = None;
        self.rebase = None;
        self.show_local_changes = false;
        if self.selected == Some(row) && self.stash_view == stash {
            if hash != WIP_HASH {
                self.clear_diff(ctx);
            }
            return;
        }
        self.selected = Some(row);
        self.stash_view = stash;
        if matches!(
            self.diff,
            Some(DiffView {
                key: DiffKey::Commit { .. } | DiffKey::Range { .. },
                ..
            })
        ) || hash != WIP_HASH
        {
            self.clear_diff(ctx);
        }
        if hash == WIP_HASH {
            self.detail = None;
            self.detail_hash = None;
            return;
        }
        self.load_detail(hash, ctx);
    }

    fn load_detail(&mut self, hash: String, ctx: &egui::Context) {
        self.detail = None;
        self.reword = None;
        self.detail_error = None;
        self.detail_hash = Some(hash.clone());
        self.detail_seq = self.detail_seq.wrapping_add(1);
        let seq = self.detail_seq;
        let path = self.path.clone();
        spawn(&self.tx, ctx, move || {
            let detail = git::commit_detail(&path, &hash);
            Msg::Detail(seq, hash, detail)
        });
    }

    fn select_stash(&mut self, i: usize, ctx: &egui::Context) {
        let Some(stash) = self.stashes.get(i) else {
            return;
        };
        let hash = stash.hash.clone();
        self.stash_view = Some(i);
        self.show_local_changes = false;
        self.selected = self.index.get(&hash).copied();
        self.scroll_to = self.selected;
        self.file_view = None;
        self.compare = None;
        self.clear_diff(ctx);
        self.load_detail(hash, ctx);
    }

    fn select_hash(&mut self, hash: &str, ctx: &egui::Context) {
        match self.index.get(hash).copied() {
            Some(row) => {
                self.select(row, ctx);
                self.scroll_to = Some(row);
            }
            None => {
                self.status = Some((
                    format!(
                        "Commit {} is not in the loaded history",
                        hash.get(..8).unwrap_or(hash)
                    ),
                    true,
                ));
            }
        }
    }

    /// Selects `hash`, loading more history first if it's further down.
    fn goto(&mut self, hash: &str, ctx: &egui::Context) {
        if self.index.contains_key(hash) {
            self.pending_goto = None;
            self.select_hash(hash, ctx);
        } else if self.has_more && self.history.len() < MAX_HISTORY {
            self.pending_goto = Some(hash.to_owned());
            self.load_more(ctx);
        } else {
            self.pending_goto = None;
            self.select_hash(hash, ctx);
        }
    }

    /// Shows a diff (or the conflict editor) in the central area; asking for the open one
    /// closes it.
    fn open_diff(&mut self, key: DiffKey, ctx: &egui::Context) {
        if self.diff.as_ref().is_some_and(|d| d.key == key) {
            self.clear_diff(ctx);
            return;
        }
        self.rebase = None;
        self.clear_diff(ctx);
        self.conflict = match &key {
            DiffKey::Work(file) if file.area == git::Area::Conflicted => {
                conflict::ConflictEditor::open(&self.path, &file.path, self.work.in_progress)
            }
            _ => None,
        };
        self.diff = Some(DiffView::new(key));
        self.refresh_diff(ctx);
    }

    /// Reads the open diff again, showing the old one meanwhile.
    fn refresh_diff(&mut self, ctx: &egui::Context) {
        let Some(view) = &mut self.diff else { return };
        let request = view.next_request();
        let key = view.key.clone();
        let path = self.path.clone();
        let opts = git::DiffOptions {
            ignore_ws: self.diff_ignore_ws,
            whole_file: Settings::get(ctx).diff_whole_file,
        };
        let dark = Tokens::get(ctx).dark();
        spawn(&self.tx, ctx, move || {
            let result = diff::load(&path, &key, opts, dark);
            Msg::Diff(request, result)
        });
    }

    /// The theme changed: the open diff is coloured again.
    pub(crate) fn recolor(&mut self, ctx: &egui::Context) {
        self.refresh_diff(ctx);
    }

    /// Starts comparing two points of history (replacing the comparison shown).
    fn compare(&mut self, compare: Compare, ctx: &egui::Context) {
        if matches!(
            self.diff,
            Some(DiffView {
                key: DiffKey::Range { .. },
                ..
            })
        ) {
            self.clear_diff(ctx);
        }
        self.file_view = None;
        self.show_local_changes = false;
        self.compare = Some(compare);
        self.reload_compare(ctx);
    }

    /// Reads the comparison's files again (the working tree or a branch moved).
    fn reload_compare(&mut self, ctx: &egui::Context) {
        self.compare_requests += 1;
        let request = self.compare_requests;
        let Some(c) = &mut self.compare else { return };
        c.request = request;
        let (path, base, target, since_fork) = (
            self.path.clone(),
            c.base.clone(),
            c.target.clone(),
            c.since_fork,
        );
        spawn(&self.tx, ctx, move || {
            let result = (|| {
                let base = git::resolve(&path, &base)?;
                let to = target.map(|t| git::resolve(&path, &t)).transpose()?;
                // The working tree goes with HEAD's history.
                let other = match &to {
                    Some(to) => to.clone(),
                    None => git::resolve(&path, "HEAD")?,
                };
                let from = if since_fork {
                    git::merge_base(&path, &base, &other)?
                } else {
                    base.clone()
                };
                Ok(CompareData {
                    files: git::diff_files(&path, &from, to.as_deref())?,
                    apart: git::count_apart(&path, &base, &other)?,
                    from,
                    to,
                })
            })();
            Msg::Compare(request, result)
        });
    }

    /// Shows `path`'s history or blame, loading what that tab needs.
    fn open_file_view(
        &mut self,
        path: String,
        rev: Option<String>,
        tab: file_view::FileTab,
        ctx: &egui::Context,
    ) {
        self.rebase = None;
        self.clear_diff(ctx);
        let view = match self.file_view.take() {
            Some(mut v) if v.path == path && v.rev == rev => {
                v.tab = tab;
                v
            }
            _ => file_view::FileView::new(&path, rev.clone(), tab),
        };
        let needs = view.needs(tab);
        self.file_view = Some(view);
        if !needs {
            return;
        }
        self.file_seq = self.file_seq.wrapping_add(1);
        let seq = self.file_seq;
        let repo = self.path.clone();
        match tab {
            file_view::FileTab::History => spawn(&self.tx, ctx, move || {
                let result = git::file_history(&repo, rev.as_deref(), &path);
                Msg::FileHistory(seq, path, result)
            }),
            file_view::FileTab::Blame => {
                let dark = Tokens::get(ctx).dark();
                spawn(&self.tx, ctx, move || {
                    let result = file_view::load_blame(&repo, rev.as_deref(), &path, dark);
                    Msg::Blame(seq, path, rev, result)
                })
            }
        }
    }

    fn reopen_diff(&mut self, ctx: &egui::Context) {
        if let Some(view) = self.diff.take() {
            self.conflict = None;
            self.open_diff(view.key, ctx);
        }
    }

    /// Runs git commands one after another (stopping at the first failure), then refreshes.
    fn run_ops(
        &mut self,
        label: &str,
        ops: Vec<Vec<String>>,
        refresh: Refresh,
        kind: OpKind,
        ctx: &egui::Context,
    ) {
        self.run_ops_ex(label, ops, refresh, kind, false, false, ctx);
    }

    fn op_remote_url(&self, ops: &[Vec<String>]) -> String {
        for op in ops {
            let cmd = op.first().map(String::as_str).unwrap_or("");
            if matches!(cmd, "fetch" | "pull" | "push" | "ls-remote") {
                for arg in op.iter().skip(1) {
                    if arg.starts_with('-') {
                        continue;
                    }
                    if arg.contains("://") || arg.contains('@') {
                        return arg.clone();
                    }
                    if let Some(r) = self.remotes.iter().find(|r| r.name == *arg) {
                        return r.url.clone();
                    }
                    if let Some((remote, _)) = arg.split_once('/')
                        && let Some(r) = self.remotes.iter().find(|r| r.name == remote)
                    {
                        return r.url.clone();
                    }
                }
            }
        }
        if let Some(cur) = self.refs.current.as_deref()
            && let Some(b) = self.refs.local.iter().find(|b| b.name == cur)
            && !b.upstream.is_empty()
            && let Some((remote, _)) = b.upstream.split_once('/')
            && let Some(r) = self.remotes.iter().find(|r| r.name == remote)
        {
            return r.url.clone();
        }
        self.default_remote()
            .and_then(|name| self.remotes.iter().find(|r| r.name == name))
            .map(|r| r.url.clone())
            .unwrap_or_default()
    }

    /// [`run_undoable`](Self::run_undoable) with the uncommitted changes stashed first and
    /// applied again afterwards, whether the operation worked or not.
    fn run_undoable_autostash(
        &mut self,
        label: &str,
        ops: Vec<Vec<String>>,
        undo: Undo,
        ctx: &egui::Context,
    ) {
        let before = self.refs.head.clone();
        let autostash = !self.work.is_clean();
        self.run_ops_ex(
            label,
            ops,
            Refresh::All,
            OpKind::Normal(undo, before),
            false,
            autostash,
            ctx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn run_ops_ex(
        &mut self,
        label: &str,
        ops: Vec<Vec<String>>,
        refresh: Refresh,
        kind: OpKind,
        quiet: bool,
        autostash: bool,
        ctx: &egui::Context,
    ) {
        if self.busy_op.is_some() {
            return;
        }
        self.prompt = None;
        let remote_url = self.op_remote_url(&ops);
        let cred_key = credential_key(&remote_url);
        let creds = self.credentials.get(&cred_key).cloned();
        self.busy_op = Some(label.to_owned());
        self.op_started = Some(Instant::now());
        if !quiet {
            self.status = None;
            self.last_op = Some(PendingOp {
                label: label.to_owned(),
                ops: ops.clone(),
                refresh,
                kind: kind.clone(),
                quiet,
                remote_url,
                autostash,
            });
        }
        let path = self.path.clone();
        let label = label.to_owned();
        spawn(&self.tx, ctx, move || {
            let mut result = Ok(String::new());
            let mut failed_step = None;
            let mut stashed = false;
            if autostash {
                match git::run(Some(&path), &["stash", "push", "-m", AUTOSTASH]) {
                    Ok(out) => stashed = !out.contains("No local changes"),
                    Err(e) => result = Err(format!("could not stash your changes: {e}")),
                }
            }
            for (idx, op) in ops.iter().enumerate() {
                if result.is_err() {
                    break;
                }
                let args: Vec<&str> = op.iter().map(String::as_str).collect();
                result = if quiet {
                    git::run_quiet_with(Some(&path), &args, creds.as_ref())
                } else {
                    git::run_with(Some(&path), &args, creds.as_ref())
                };
                if result.is_err() {
                    failed_step = Some(idx);
                }
            }
            // The changes come back however it went; staged ones staged again when possible.
            if stashed {
                let pop = git::run(Some(&path), &["stash", "pop", "--index"])
                    .or_else(|_| git::run(Some(&path), &["stash", "pop"]));
                if let Err(e) = pop {
                    result = Err(match result {
                        Ok(_) => format!("{AUTOSTASH_CONFLICT}: {e}"),
                        Err(before) => format!("{before}\n{AUTOSTASH_CONFLICT}: {e}"),
                    });
                }
            }
            // For HEAD moves, the new HEAD is what redo goes back to.
            let kind = match kind {
                OpKind::Normal(Undo::HeadMove { mode }, before) if result.is_ok() => {
                    OpKind::Normal(
                        Undo::HeadMove { mode },
                        format!("{before}\u{0}{}", git::head(&path)),
                    )
                }
                other => other,
            };
            Msg::OpDone {
                label,
                result,
                refresh,
                kind,
                quiet,
                failed_step,
            }
        });
    }

    /// Runs `ops` as an undoable step.
    fn run_undoable(
        &mut self,
        label: &str,
        ops: Vec<Vec<String>>,
        refresh: Refresh,
        undo: Undo,
        ctx: &egui::Context,
    ) {
        let before = self.refs.head.clone();
        self.run_ops(label, ops, refresh, OpKind::Normal(undo, before), ctx);
    }

    fn checkout(&mut self, target: &str, ctx: &egui::Context) {
        if self.busy_op.is_some() {
            return;
        }
        if target.is_empty() || target.starts_with('-') {
            self.status = Some(("Invalid checkout target".to_owned(), true));
            return;
        }
        self.pending_checkout = Some(branch_ops::CheckoutTarget::Branch(target.to_owned()));
        let previous = self
            .refs
            .current
            .clone()
            .unwrap_or_else(|| self.refs.head.clone());
        let undo = if previous.is_empty() {
            Undo::None
        } else {
            Undo::Custom {
                undo: ops(&[&["checkout", &previous]]),
                redo: ops(&[&["checkout", target]]),
            }
        };
        self.run_undoable(
            &format!("Checkout {target}"),
            ops(&[&["checkout", target]]),
            Refresh::All,
            undo,
            ctx,
        );
    }

    /// Moves `branch` to `target`, further on: checks it out if it isn't, and fast-forwards
    /// it with the uncommitted changes stashed around. Branches that have diverged get the
    /// choice of rebasing, merging or resetting in the toolbar instead. Undoing goes back and
    /// puts the branch where it was.
    fn move_branch(&mut self, branch: &str, target: &str, ctx: &egui::Context) {
        if self.busy_op.is_some() {
            return;
        }
        if branch.is_empty()
            || branch.starts_with('-')
            || target.is_empty()
            || target.starts_with('-')
        {
            self.status = Some(("Invalid branch name".to_owned(), true));
            return;
        }
        let Some(was) = self
            .refs
            .local
            .iter()
            .find(|b| b.name == branch)
            .map(|b| b.target.clone())
        else {
            return self.checkout(branch, ctx);
        };
        let current = self.refs.current.as_deref() == Some(branch);
        let (only_here, only_there) = self.divergence(branch, target);
        if only_here > 0 {
            self.prompt = Some(self.diverged_prompt(branch, target, only_here, only_there));
            return;
        }
        if !current {
            // Offered again with the changes stashed or thrown away when they block the checkout.
            self.pending_checkout = Some(branch_ops::CheckoutTarget::Move {
                branch: branch.to_owned(),
                target: target.to_owned(),
            });
        }
        let mut run = Vec::new();
        if !current {
            run.push(vec!["checkout".to_owned(), branch.to_owned()]);
        }
        run.push(vec![
            "merge".to_owned(),
            "--ff-only".to_owned(),
            target.to_owned(),
        ]);
        let (label, undo) = if current {
            (
                format!("Fast-forward {branch} to {target}"),
                Undo::HeadMove { mode: "--keep" },
            )
        } else {
            (
                format!("Checkout {branch} at {target}"),
                self.back_to_previous(branch, &was, run.clone()),
            )
        };
        self.run_undoable_autostash(&label, run, undo, ctx);
    }

    /// Undoing a move of `branch` that checked it out: back to the branch before, and
    /// `branch` back at `was`.
    fn back_to_previous(&self, branch: &str, was: &str, redo: Vec<Vec<String>>) -> Undo {
        let previous = self
            .refs
            .current
            .clone()
            .unwrap_or_else(|| self.refs.head.clone());
        if previous.is_empty() {
            return Undo::None;
        }
        Undo::Custom {
            undo: ops(&[&["checkout", &previous], &["branch", "-f", branch, was]]),
            redo,
        }
    }

    /// Commits only on `a`, and only on `b`.
    fn divergence(&self, a: &str, b: &str) -> (u32, u32) {
        let range = format!("{a}...{b}");
        git::run(
            Some(&self.path),
            &["rev-list", "--left-right", "--count", &range],
        )
        .ok()
        .and_then(|out| {
            let (l, r) = out.trim().split_once(char::is_whitespace)?;
            Some((l.trim().parse().ok()?, r.trim().parse().ok()?))
        })
        .unwrap_or((0, 0))
    }

    /// What can be done when `branch` can't simply move to `target`: rebase it, merge, or
    /// reset it there (git's own `--autostash` keeps the changes through each).
    fn diverged_prompt(&self, branch: &str, target: &str, here: u32, there: u32) -> Prompt {
        let current = self.refs.current.as_deref() == Some(branch);
        let was = self
            .refs
            .local
            .iter()
            .find(|b| b.name == branch)
            .map(|b| b.target.clone())
            .unwrap_or_default();
        let commits = |n: u32| {
            if n == 1 {
                "1 commit".to_owned()
            } else {
                format!("{n} commits")
            }
        };
        let option = |label: String, tip: String, step: &[&str], danger: bool| {
            let mut run = Vec::new();
            if !current {
                run.push(vec!["checkout".to_owned(), branch.to_owned()]);
            }
            run.push(step.iter().map(|s| (*s).to_owned()).collect());
            let undo = if current {
                Undo::HeadMove { mode: "--keep" }
            } else {
                self.back_to_previous(branch, &was, run.clone())
            };
            let action = Action::UndoableOps {
                label: label.clone(),
                ops: run,
                refresh: Refresh::All,
                undo,
            };
            (label, tip, action, danger)
        };
        Prompt {
            text: format!(
                "{branch} and {target} have diverged: {} only on {branch}, {} only on {target}.",
                commits(here),
                commits(there)
            ),
            options: vec![
                option(
                    format!("Rebase {branch} onto {target}"),
                    format!("Replays {branch}'s {} on top of {target}", commits(here)),
                    &["rebase", "--autostash", target],
                    false,
                ),
                option(
                    format!("Merge {target} into {branch}"),
                    "A merge commit joins the two".to_owned(),
                    &["merge", "--autostash", "--no-edit", target],
                    false,
                ),
                option(
                    format!("Reset {branch} to {target}"),
                    format!(
                        "Moves {branch} to {target}, leaving out its {}; Undo brings them back",
                        commits(here)
                    ),
                    &["reset", "--keep", target],
                    true,
                ),
            ],
        }
    }

    /// The latest commit is amended (its message only, whatever is staged); an older one is
    /// reworded by an interactive rebase from its parent.
    fn reword_commit(&mut self, hash: String, message: String, ctx: &egui::Context) {
        if self.busy_op.is_some() {
            return;
        }
        if self.work.in_progress.is_some() {
            self.status = Some(("Finish the operation in progress before editing a commit message".to_owned(), true));
            return;
        }
        self.reword = None;
        if hash == self.refs.head {
            let (subject, body) = message.split_once("\n\n").unwrap_or((&message, ""));
            let mut amend = vec![
                "commit",
                "--amend",
                "--only",
                "--allow-empty",
                "-m",
                subject,
            ];
            if !body.is_empty() {
                amend.extend(["-m", body]);
            }
            self.run_undoable(
                &format!("Edit the message of {}", short(&hash)),
                ops(&[&amend]),
                Refresh::All,
                Undo::HeadMove { mode: "--keep" },
                ctx,
            );
            return;
        }
        self.quick_edit(hash, QuickEdit::Reword(message), ctx);
    }

    /// Lists the commits a quick edit of `hash` replays (from its parent; from its
    /// grandparent to move it down), then runs the rebase (see `Msg::QuickEditCommits`).
    fn quick_edit(&mut self, hash: String, edit: QuickEdit, ctx: &egui::Context) {
        if self.busy_op.is_some() {
            return;
        }
        if self.work.in_progress.is_some() {
            self.status = Some((
                "Finish the operation in progress before editing a commit".to_owned(),
                true,
            ));
            return;
        }
        // The latest commit dropped: a reset, which keeps the uncommitted changes.
        if matches!(edit, QuickEdit::Drop) && hash == self.refs.head {
            self.run_undoable(
                &format!("Drop {}", short(&hash)),
                ops(&[&["reset", "--keep", &format!("{hash}^")]]),
                Refresh::All,
                Undo::HeadMove { mode: "--keep" },
                ctx,
            );
            return;
        }
        // The rebase brings uncommitted changes back unstaged (`--autostash`): a careful partial
        // staging would be flattened. Ask for it to be committed or unstaged first.
        if !self.work.staged.is_empty() {
            self.status = Some((
                "Rewriting an older commit would unstage your staged changes: commit or unstage them first".to_owned(),
                true,
            ));
            return;
        }
        let onto = match edit {
            QuickEdit::MoveDown => format!("{hash}~2"),
            _ => format!("{hash}^"),
        };
        self.rebase_seq = self.rebase_seq.wrapping_add(1);
        let seq = self.rebase_seq;
        let path = self.path.clone();
        spawn(&self.tx, ctx, move || {
            let result = git::rebase_commits(&path, &onto);
            Msg::QuickEditCommits {
                seq,
                hash,
                edit,
                result,
            }
        });
    }

    fn checkout_remote(&mut self, remote_branch: &str, ctx: &egui::Context) {
        if self.busy_op.is_some() {
            return;
        }
        if remote_branch.is_empty() || remote_branch.starts_with('-') {
            self.status = Some(("Invalid remote branch".to_owned(), true));
            return;
        }
        // `origin/feature` → local `feature` tracking it (or just switch if it exists).
        let local = remote_branch
            .split_once('/')
            .map(|(_, b)| b)
            .unwrap_or(remote_branch)
            .to_owned();
        if local.is_empty() || local.starts_with('-') {
            self.status = Some(("Invalid branch name".to_owned(), true));
            return;
        }
        if self.refs.local.iter().any(|b| b.name == local) {
            self.checkout(&local, ctx);
            return;
        }
        self.pending_checkout = Some(branch_ops::CheckoutTarget::Remote {
            remote_branch: remote_branch.to_owned(),
            local: local.clone(),
        });
        let previous = self
            .refs
            .current
            .clone()
            .unwrap_or_else(|| self.refs.head.clone());
        let undo = if previous.is_empty() {
            Undo::None
        } else {
            Undo::Custom {
                undo: ops(&[&["checkout", &previous], &["branch", "-D", &local]]),
                redo: ops(&[&["checkout", "--track", remote_branch]]),
            }
        };
        self.run_undoable(
            &format!("Checkout {local}"),
            ops(&[&["checkout", "--track", remote_branch]]),
            Refresh::All,
            undo,
            ctx,
        );
    }

    /// Once per tab, on a GitHub / GitLab repository: tells the avatar loader where its authors'
    /// account pictures are.
    fn hint_avatars(&mut self, ctx: &egui::Context) {
        if self.avatars_hinted || !Settings::get(ctx).avatars {
            return;
        }
        let Some(forge) = self.forge else { return };
        let Some((host, path)) = self
            .remotes
            .iter()
            .find(|r| r.name == "origin")
            .or_else(|| self.remotes.first())
            .and_then(|r| forge::location(&r.url))
        else {
            return;
        };
        self.avatars_hinted = true;
        let repo = self.path.clone();
        let mut emails: Vec<String> = self.history.iter().map(|c| c.email.clone()).collect();
        emails.sort();
        emails.dedup();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            match forge {
                Forge::GitHub => {
                    for (email, url) in forge::github_avatars(&repo, &host, &path) {
                        crate::avatars::hint(&email, crate::avatars::Hint::Url(url));
                    }
                }
                Forge::GitLab => {
                    for email in emails {
                        crate::avatars::hint(&email, crate::avatars::Hint::GitLab(host.clone()));
                    }
                }
            }
            ctx.request_repaint();
        });
    }

    /// HTTPS credentials for this tab's git commands, scoped to `remote_url`'s origin.
    pub fn set_credentials(&mut self, remote_url: &str, creds: git::Credentials) {
        self.credentials.insert(credential_key(remote_url), creds);
    }

    fn default_remote(&self) -> Option<String> {
        self.remotes
            .iter()
            .map(|r| r.name.as_str())
            .find(|r| *r == "origin")
            .or_else(|| self.remotes.first().map(|r| r.name.as_str()))
            .map(str::to_owned)
    }

    fn push(&mut self, ctx: &egui::Context) {
        let Some(current) = self.refs.current.clone() else {
            self.status = Some(("Push needs a checked-out branch".to_owned(), true));
            return;
        };
        let has_upstream = self
            .refs
            .local
            .iter()
            .any(|b| b.name == current && !b.upstream.is_empty() && !b.upstream_gone());
        if has_upstream {
            self.run_ops(
                "Push",
                ops(&[&["push"]]),
                Refresh::All,
                OpKind::Normal(Undo::None, String::new()),
                ctx,
            );
            return;
        }
        // First push of a branch: publish it and track the remote one.
        match self.default_remote() {
            Some(remote) => self.run_ops(
                &format!("Push {current} to {remote}"),
                ops(&[&["push", "-u", &remote, &current]]),
                Refresh::All,
                OpKind::Normal(Undo::None, String::new()),
                ctx,
            ),
            None => {
                self.status = Some(("This repository has no remote to push to".to_owned(), true))
            }
        }
    }

    fn commit(&mut self, ctx: &egui::Context) {
        if !self.work.conflicted.is_empty() {
            self.status = Some(("Resolve the conflicted files first".to_owned(), true));
            return;
        }
        // A rebase stopped on conflicts finishes with `rebase --continue`, not a new commit.
        if self.work.in_progress == Some(InProgress::Rebase) {
            self.run_ops(
                "Continue rebase",
                vec![vec!["rebase".to_owned(), "--continue".to_owned()]],
                Refresh::All,
                OpKind::Normal(Undo::None, String::new()),
                ctx,
            );
            return;
        }
        let summary = self.commit_summary.trim();
        if summary.is_empty() {
            self.status = Some(("Write a commit summary first".to_owned(), true));
            return;
        }
        let amend = self.amend && self.work.in_progress.is_none() && !self.refs.head.is_empty();
        if self.work.in_progress.is_some() {
            self.amend = false;
        }
        let mut commit = vec!["commit".to_owned()];
        if amend {
            commit.push("--amend".to_owned());
        }
        if self.no_verify {
            commit.push("--no-verify".to_owned());
        }
        commit.extend(["-m".to_owned(), summary.to_owned()]);
        let body = self.commit_body.trim();
        if !body.is_empty() {
            commit.extend(["-m".to_owned(), body.to_owned()]);
        }
        let mut steps = Vec::new();
        // Nothing staged: commit everything, like "Stage all and commit" (an amend with
        // nothing staged only rewrites the message).
        if self.work.staged.is_empty() && !amend {
            steps.push(vec!["add".to_owned(), "-A".to_owned()]);
        }
        steps.push(commit);
        let label = if amend { "Amend" } else { "Commit" };
        self.run_undoable(
            label,
            steps,
            Refresh::All,
            Undo::HeadMove { mode: "--soft" },
            ctx,
        );
    }

    fn stash_push(&mut self, message: &str, ctx: &egui::Context) {
        let mut args = vec!["stash", "push", "--include-untracked"];
        if !message.trim().is_empty() {
            args.extend(["-m", message.trim()]);
        }
        let redo = ops(&[&args]);
        self.run_undoable(
            "Stash",
            ops(&[&args]),
            Refresh::All,
            Undo::Custom {
                undo: ops(&[&["stash", "pop", "--index"]]),
                redo,
            },
            ctx,
        );
    }

    fn undo(&mut self, ctx: &egui::Context) {
        if self.busy_op.is_some() {
            return;
        }
        if let Some(entry) = self.undo_stack.pop() {
            let label = format!("Undo {}", entry.label);
            let steps = entry.undo.clone();
            self.run_ops(&label, steps, Refresh::All, OpKind::Undo(entry), ctx);
        }
    }

    fn redo(&mut self, ctx: &egui::Context) {
        if self.busy_op.is_some() {
            return;
        }
        if let Some(entry) = self.redo_stack.pop() {
            let label = format!("Redo {}", entry.label);
            let steps = entry.redo.clone();
            self.run_ops(&label, steps, Refresh::All, OpKind::Redo(entry), ctx);
        }
    }

    fn run_search(&mut self, ctx: &egui::Context) {
        let query = self.search.query.trim().to_owned();
        self.search.edited = None;
        if query.is_empty() {
            self.search.results.clear();
            self.search.matches.clear();
            self.search.searched.clear();
            return;
        }
        self.search_seq = self.search_seq.wrapping_add(1);
        let seq = self.search_seq;
        self.search.running = true;
        let path = self.path.clone();
        let code = self.search.code;
        spawn(&self.tx, ctx, move || {
            let result = if code {
                git::search_code(&path, &query)
            } else {
                git::search(&path, &query)
            };
            Msg::Search(seq, query, result)
        });
    }

    fn search_step(&mut self, delta: i32, ctx: &egui::Context) {
        let n = self.search.results.len();
        if n == 0 {
            return;
        }
        let cur = self.search.current as i32 + delta;
        self.search.current = cur.rem_euclid(n as i32) as usize;
        if let Some(hash) = self.search.results.get(self.search.current).cloned() {
            self.goto(&hash, ctx);
        }
    }

    fn start_terminal(
        &mut self,
        shell: Option<ShellSpec>,
        cwd: Option<PathBuf>,
        ctx: &egui::Context,
    ) {
        let shell = shell.unwrap_or_else(|| crate::terminal::default_shell(ctx));
        let cwd = cwd.unwrap_or_else(|| self.path.clone());
        match Terminal::spawn(&cwd, &shell, ctx) {
            Ok(term) => {
                self.terminals.push(term);
                self.active_terminal = self.terminals.len() - 1;
                self.terminal_error = None;
                self.terminal_open = true;
            }
            Err(e) => {
                self.terminal_error = Some(e);
                self.terminal_open = true;
            }
        }
    }

    pub(super) fn pick_worktree_dir(&mut self, ctx: &egui::Context) {
        if self.file_picker.is_some() {
            return;
        }
        let dialog = rfd::AsyncFileDialog::new().set_title("Worktree folder");
        self.file_picker = Some((
            RepoPicker::WorktreeDir,
            crate::file_dialog::PendingDialog::pick_folder(dialog, ctx),
        ));
    }

    pub(super) fn pick_apply_patch(&mut self, ctx: &egui::Context) {
        if self.file_picker.is_some() {
            return;
        }
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("Choose a patch")
            .add_filter("Patch", &["patch", "diff", "txt"]);
        self.file_picker = Some((
            RepoPicker::ApplyPatch,
            crate::file_dialog::PendingDialog::pick_file(dialog, ctx),
        ));
    }

    pub(super) fn pick_save_patch(
        &mut self,
        source: patch::PatchSource,
        suggested: &str,
        ctx: &egui::Context,
    ) {
        if self.file_picker.is_some() {
            return;
        }
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("Save patch")
            .set_file_name(suggested)
            .add_filter("Patch", &["patch", "diff"]);
        self.file_picker = Some((
            RepoPicker::SavePatch(source),
            crate::file_dialog::PendingDialog::save_file(dialog, ctx),
        ));
    }

    fn poll_file_picker(&mut self, ctx: &egui::Context) {
        let Some((target, mut pending)) = self.file_picker.take() else {
            return;
        };
        let Some(picked) = pending.poll() else {
            self.file_picker = Some((target, pending));
            return;
        };
        if let Some(path) = picked {
            match target {
                RepoPicker::WorktreeDir => {
                    if let Some(Dialog::AddWorktree {
                        path: wt_path,
                        path_edited,
                        ..
                    }) = &mut self.dialog
                    {
                        *wt_path = path.display().to_string();
                        *path_edited = true;
                    }
                }
                RepoPicker::ApplyPatch => {
                    if let Some(Dialog::Apply(apply)) = &mut self.dialog {
                        apply.file = path.display().to_string();
                    }
                }
                RepoPicker::SavePatch(source) => self.save_patch_to(source, path, ctx),
            }
        }
    }

    /// Drains finished background work and file-system events.
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg, ctx);
        }
        self.poll_file_picker(ctx);
        if !self.terminals.is_empty() {
            self.active_terminal = self.active_terminal.min(self.terminals.len() - 1);
        }

        // Changes on disk, debounced.
        if let Some(rx) = &self.fs_rx {
            while let Ok(git_dir) = rx.try_recv() {
                let was = self.fs_pending.map(|(g, _)| g).unwrap_or(false);
                self.fs_pending = Some((was || git_dir, Instant::now()));
            }
        }
        if let Some((git_dir, at)) = self.fs_pending {
            if at.elapsed() > Duration::from_millis(400)
                && !self.loading
                && self.busy_op.is_none()
            {
                self.fs_pending = None;
                if git_dir {
                    self.reload(ctx);
                } else {
                    self.reload_status(ctx);
                }
            } else {
                // Keep waking until the debounce window closes and no op/load blocks reload.
                ctx.request_repaint_after(Duration::from_millis(200));
            }
        }

        // Background fetch (paused while a modal, rebase, conflict editor, or op is active).
        let minutes = Settings::get(ctx).auto_fetch_minutes;
        let blocked = self.has_dialog()
            || self.busy_op.is_some()
            || self.loading
            || self.rebase.is_some()
            || self.conflict.is_some()
            || self.work.in_progress.is_some()
            || self.push_follow != ext::PushFollow::Idle;
        if minutes > 0
            && !self.remotes.is_empty()
            && !blocked
            && self.last_fetch.elapsed() > Duration::from_secs(u64::from(minutes) * 60)
        {
            self.last_fetch = Instant::now();
            self.run_ops_ex(
                "Background fetch",
                ops(&[&["fetch", "--all", "--prune", "--quiet"]]),
                Refresh::All,
                OpKind::Normal(Undo::None, String::new()),
                true,
                false,
                ctx,
            );
        }
        if minutes > 0 {
            ctx.request_repaint_after(Duration::from_secs(30));
        }

        // Debounced search while typing.
        if let Some(at) = self.search.edited
            && at.elapsed() > Duration::from_millis(350)
        {
            self.run_search(ctx);
        } else if self.search.edited.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn handle(&mut self, msg: Msg, ctx: &egui::Context) {
        match msg {
            Msg::Loaded(seq, result) => {
                if seq != self.load_seq {
                    return;
                }
                self.loading = false;
                match result {
                    Ok(loaded) => {
                        self.load_error = None;
                        self.refs = loaded.refs;
                        self.has_more = loaded.history.len() >= loaded.requested;
                        self.history = loaded.history;
                        self.work = loaded.work;
                        self.prune_patch_selection();
                        self.worktrees = loaded.worktrees;
                        self.submodules = loaded.submodules;
                        // Stash numbers shift when one is pushed or dropped; follow the commit.
                        let viewed = self
                            .stash_view
                            .and_then(|i| self.stashes.get(i))
                            .map(|s| s.hash.clone());
                        self.stashes = loaded.stashes;
                        self.stash_view =
                            viewed.and_then(|h| self.stashes.iter().position(|s| s.hash == h));
                        self.identity = loaded.identity;
                        self.head_message = loaded.head_message;
                        self.bisect = loaded.bisect;
                        self.stored_issue_rules = loaded.issue_rules;
                        self.forget_issue_rules(ctx);
                        let forge = forge::detect(&loaded.remotes);
                        self.remotes = loaded.remotes;
                        if forge != self.forge {
                            self.forge = forge;
                            self.prs.clear();
                            self.prs_loaded = None;
                        }
                        if self
                            .prs_loaded
                            .is_none_or(|t| t.elapsed() > Duration::from_secs(120))
                        {
                            self.load_prs(ctx);
                        }
                        self.rebuild(ctx);
                        self.hint_avatars(ctx);
                        self.reload_compare(ctx);
                    }
                    Err(e) => self.load_error = Some(e),
                }
                if std::mem::take(&mut self.reload_queued) {
                    self.reload(ctx);
                }
            }
            Msg::More(result) => {
                self.loading_more = false;
                match result {
                    Ok(page) => {
                        self.has_more = page.len() == git::LOG_PAGE;
                        // A reload in between may have loaded these already.
                        let known: HashSet<&str> =
                            self.history.iter().map(|c| c.hash.as_str()).collect();
                        let fresh: Vec<Commit> = page
                            .into_iter()
                            .filter(|c| !known.contains(c.hash.as_str()))
                            .collect();
                        self.history.extend(fresh);
                        self.rebuild(ctx);
                    }
                    Err(e) => self.status = Some((e, true)),
                }
            }
            Msg::Status(seq, result) => {
                if seq != self.status_seq {
                    return;
                }
                match result {
                    Ok(work) => {
                        self.work = work;
                        self.prune_patch_selection();
                        self.rebuild(ctx);
                        if self.compare.as_ref().is_some_and(|c| c.target.is_none()) {
                            self.reload_compare(ctx);
                        }
                    }
                    Err(e) => self.status = Some((e, true)),
                }
            }
            Msg::Detail(seq, hash, result) => {
                if seq == self.detail_seq && self.detail_hash.as_deref() == Some(hash.as_str()) {
                    match result {
                        Ok(d) => self.detail = Some(d),
                        Err(e) => self.detail_error = Some(e),
                    }
                    if self.pending_edit.take().as_deref() == Some(hash.as_str()) {
                        self.open_message_editor();
                    }
                }
            }
            Msg::RebaseCommits {
                seq,
                onto,
                label,
                result,
            } => {
                if seq != self.rebase_seq || self.work.in_progress.is_some() {
                    return;
                }
                match result {
                    Ok(commits) if commits.is_empty() => {
                        self.status = Some((
                            format!(
                                "Nothing to rebase: {label} already contains every commit of this branch"
                            ),
                            true,
                        ));
                    }
                    Ok(commits) => {
                        self.clear_diff(ctx);
                        self.file_view = None;
                        self.rebase = Some(rebase::RebaseEditor::new(onto, label, commits));
                    }
                    Err(e) => self.status = Some((format!("Interactive rebase: {e}"), true)),
                }
            }
            Msg::QuickEditCommits {
                seq,
                hash,
                edit,
                result,
            } => {
                if seq != self.rebase_seq
                    || self.busy_op.is_some()
                    || self.work.in_progress.is_some()
                    || !self.work.staged.is_empty()
                {
                    return;
                }
                let label = match &edit {
                    QuickEdit::Reword(_) => format!("Edit the message of {}", short(&hash)),
                    QuickEdit::Drop => format!("Drop {}", short(&hash)),
                    QuickEdit::MoveUp => format!("Move {} up", short(&hash)),
                    QuickEdit::MoveDown => format!("Move {} down", short(&hash)),
                };
                let result = result.and_then(|commits| {
                    let onto = match edit {
                        QuickEdit::MoveDown => format!("{hash}~2"),
                        _ => format!("{hash}^"),
                    };
                    let mut editor =
                        rebase::RebaseEditor::new(onto, short(&hash).to_owned(), commits);
                    let i = editor
                        .entries
                        .iter()
                        .position(|e| e.commit.hash == hash)
                        .ok_or("only commits of the checked-out branch can be edited")?;
                    match edit {
                        QuickEdit::Reword(message) => {
                            if let Some(entry) = editor.entries.get_mut(i) {
                                entry.step = rebase::Step::Reword;
                                entry.message = message;
                            }
                        }
                        QuickEdit::Drop => {
                            if let Some(entry) = editor.entries.get_mut(i) {
                                entry.step = rebase::Step::Drop;
                            }
                        }
                        // Oldest first: up is towards the end.
                        QuickEdit::MoveUp if i + 1 < editor.entries.len() => {
                            editor.entries.swap(i, i + 1);
                        }
                        QuickEdit::MoveDown if i > 0 => editor.entries.swap(i - 1, i),
                        QuickEdit::MoveUp | QuickEdit::MoveDown => {
                            return Err("there is no commit there to swap it with".to_owned());
                        }
                    }
                    if let Some(problem) = editor.problem() {
                        return Err(problem);
                    }
                    editor.command()
                });
                match result {
                    Ok(command) => self.run_undoable(
                        &label,
                        vec![command],
                        Refresh::All,
                        Undo::HeadMove { mode: "--keep" },
                        ctx,
                    ),
                    Err(e) => self.status = Some((format!("{label}: {e}"), true)),
                }
            }
            Msg::FileHistory(seq, path, result) => {
                if seq == self.file_seq
                    && let Some(view) = &mut self.file_view
                    && view.path == path
                {
                    view.history = Some(result);
                }
            }
            Msg::Blame(seq, path, rev, result) => {
                if seq == self.file_seq
                    && let Some(view) = &mut self.file_view
                    && view.path == path
                    && view.rev == rev
                {
                    view.blame = Some(result);
                }
            }
            Msg::Diff(request, result) => {
                if let Some(view) = &mut self.diff {
                    view.arrived(request, result);
                }
            }
            Msg::Compare(request, result) => {
                if let Some(c) = &mut self.compare
                    && c.request == request
                {
                    c.data = Some(result);
                }
            }
            Msg::Search(seq, query, result) => {
                if seq == self.search_seq && query == self.search.query.trim() {
                    self.search.running = false;
                    self.search.searched = query;
                    match result {
                        Ok(hashes) => {
                            self.search.matches = hashes.iter().cloned().collect();
                            self.search.results = hashes;
                            self.search.current = 0;
                            if let Some(first) = self.search.results.first().cloned() {
                                self.goto(&first, ctx);
                            }
                        }
                        Err(e) => self.status = Some((e, true)),
                    }
                }
            }
            Msg::Prs(seq, result) => {
                if seq == self.prs_seq {
                    match result {
                        Ok(prs) => {
                            self.prs = prs;
                            self.pr_error = None;
                        }
                        Err(e) => self.pr_error = Some(e),
                    }
                }
            }
            Msg::WatcherReady(watcher, rx) => {
                self.watcher = Some(watcher);
                self.fs_rx = Some(rx);
            }
            Msg::OpDone {
                label,
                result,
                refresh,
                kind,
                quiet,
                failed_step,
            } => {
                self.busy_op = None;
                let checkout_target = self.pending_checkout.take();
                let long = self
                    .op_started
                    .take()
                    .is_some_and(|t| t.elapsed() > LONG_OP);
                match &result {
                    Ok(_) => {
                        if !quiet {
                            self.last_op = None;
                            self.pending_pr = None;
                        }
                        if label == "Commit" || label == "Amend" || label == "Continue rebase" {
                            self.commit_summary.clear();
                            self.commit_body.clear();
                            self.amend = false;
                            self.no_verify = false;
                        }
                        // Quiet success for staging, which happens all the time.
                        if refresh == Refresh::All && !quiet {
                            self.status = Some((format!("{label}: done"), false));
                        }
                        if label.contains("etch") {
                            self.load_prs(ctx);
                        }
                        self.record_undo(&label, kind);
                    }
                    Err(e) if git::is_auth_error(e) && !quiet => {
                        if let Some(step) = failed_step
                            && let Some(op) = &mut self.last_op
                        {
                            let idx = step.min(op.ops.len());
                            if idx > 0 {
                                op.ops = op.ops.split_off(idx);
                            }
                        }
                        let url = self
                            .last_op
                            .as_ref()
                            .map(|o| o.remote_url.clone())
                            .filter(|u| !u.is_empty())
                            .or_else(|| {
                                self.pending_pr
                                    .as_ref()
                                    .map(|p| p.remote_url.clone())
                                    .filter(|u| !u.is_empty())
                            })
                            .or_else(|| {
                                self.default_remote()
                                    .and_then(|r| self.remotes.iter().find(|x| x.name == r))
                                    .map(|r| r.url.clone())
                            })
                            .unwrap_or_default();
                        let refused = self.credentials.remove(&credential_key(&url)).is_some();
                        self.status = Some((format!("{label}: sign-in needed"), true));
                        self.dialog = Some(Dialog::credentials(&url, refused));
                    }
                    Err(e) if e.contains(AUTOSTASH_CONFLICT) => {
                        self.focus_wip = true;
                        self.status = Some((format!("{label}: done"), false));
                        self.prompt = Some(Prompt {
                            text: "Your changes didn't apply cleanly on top: fix the files \
                                   marked as conflicted in Changes. They are kept in the stash \
                                   as well."
                                .to_owned(),
                            options: Vec::new(),
                        });
                        self.record_undo(&label, kind);
                    }
                    Err(e)
                        if label == "Pull"
                            && (e.contains("Not possible to fast-forward")
                                || e.contains("Diverging branches")) =>
                    {
                        let current = self.refs.current.clone().unwrap_or_default();
                        let upstream = self
                            .refs
                            .local
                            .iter()
                            .find(|b| b.name == current)
                            .map(|b| b.upstream.clone())
                            .unwrap_or_default();
                        let (here, there) = self.divergence(&current, &upstream);
                        self.prompt = Some(self.diverged_prompt(&current, &upstream, here, there));
                    }
                    Err(e) => {
                        if !quiet {
                            self.pending_pr = None;
                            if let Some(step) = failed_step
                                && let Some(op) = &mut self.last_op
                            {
                                let idx = step.min(op.ops.len());
                                if idx > 0 {
                                    op.ops = op.ops.split_off(idx);
                                }
                            }
                            if let Some(dialog) =
                                branch_ops::blocked_checkout_for(checkout_target, &label, e)
                            {
                                self.dialog = Some(dialog);
                            }
                            self.focus_wip =
                                e.contains("CONFLICT") || e.contains("could not apply");
                            self.status = Some((format!("{label} failed: {e}"), true));
                        }
                        // A failed undo stays undoable.
                        match kind {
                            OpKind::Undo(entry) => self.undo_stack.push(entry),
                            OpKind::Redo(entry) => self.redo_stack.push(entry),
                            OpKind::Normal(..) => {}
                        }
                    }
                }
                if long && !quiet {
                    self.finished = Some(match &result {
                        Ok(_) => (format!("{} · {label}: done", self.name), false),
                        Err(e) => (
                            format!("{} · {label} failed: {}", self.name, headline(e)),
                            true,
                        ),
                    });
                }
                match refresh {
                    Refresh::Status => self.reload_status(ctx),
                    Refresh::All => self.reload(ctx),
                }
                self.after_op(&label, &result, ctx);
            }
        }
    }

    fn record_undo(&mut self, label: &str, kind: OpKind) {
        match kind {
            OpKind::Normal(Undo::None, _) => {}
            OpKind::Normal(Undo::HeadMove { mode }, heads) => {
                let (before, after) = heads.split_once('\u{0}').unwrap_or(("", ""));
                if before.is_empty() || after.is_empty() || before == after {
                    return;
                }
                self.undo_stack.push(UndoEntry {
                    label: label.to_owned(),
                    undo: ops(&[&["reset", mode, before]]),
                    redo: ops(&[&["reset", mode, after]]),
                });
                self.redo_stack.clear();
            }
            OpKind::Normal(Undo::Custom { undo, redo }, _) => {
                self.undo_stack.push(UndoEntry {
                    label: label.to_owned(),
                    undo,
                    redo,
                });
                self.redo_stack.clear();
            }
            OpKind::Undo(entry) => self.redo_stack.push(entry),
            OpKind::Redo(entry) => self.undo_stack.push(entry),
        }
        // Plenty for a session.
        if self.undo_stack.len() > 50 {
            self.undo_stack.remove(0);
        }
    }

    pub(crate) fn apply(&mut self, action: Action, ctx: &egui::Context) {
        let plain = OpKind::Normal(Undo::None, String::new());
        match action {
            Action::Select(row) => self.select(row, ctx),
            Action::SelectHash(hash) => self.goto(&hash, ctx),
            Action::SelectStash(i) => self.select_stash(i, ctx),
            Action::OpenDiff(key) => self.open_diff(key, ctx),
            Action::CloseDiff => {
                self.clear_diff(ctx);
            }
            Action::Ops {
                label,
                ops,
                refresh,
            } => self.run_ops(&label, ops, refresh, plain, ctx),
            Action::UndoableOps {
                label,
                ops,
                refresh,
                undo,
            } => self.run_undoable(&label, ops, refresh, undo, ctx),
            Action::Checkout(name) => self.checkout(&name, ctx),
            Action::MoveBranch { branch, target } => self.move_branch(&branch, &target, ctx),
            Action::BranchFilter(change) => {
                self.graph_filter.apply(change);
                self.reload(ctx);
            }
            Action::CheckoutRemote(name) => self.checkout_remote(&name, ctx),
            Action::Dialog(dialog) => self.dialog = Some(dialog),
            Action::Commit => self.commit(ctx),
            Action::Fetch => {
                self.last_fetch = Instant::now();
                self.run_ops(
                    "Fetch",
                    ops(&[&["fetch", "--all", "--prune"]]),
                    Refresh::All,
                    plain,
                    ctx,
                )
            }
            Action::Pull => self.run_undoable(
                "Pull",
                ops(&[&["pull", "--ff-only", "--autostash"]]),
                Refresh::All,
                Undo::HeadMove { mode: "--keep" },
                ctx,
            ),
            Action::Push => self.push(ctx),
            Action::Reload => self.reload(ctx),
            Action::LoadMore => self.load_more(ctx),
            Action::Undo => self.undo(ctx),
            Action::Redo => self.redo(ctx),
            Action::StashPush(message) => self.stash_push(&message, ctx),
            Action::OpenTab(path) => self.open_request = Some(path),
            Action::ToggleTerminal => {
                self.terminal_open = !self.terminal_open;
                if self.terminal_open && self.terminals.is_empty() {
                    self.start_terminal(None, None, ctx);
                }
            }
            Action::NewTerminal { shell, cwd } => self.start_terminal(shell, cwd, ctx),
            Action::CloseTerminal(i) => {
                if i < self.terminals.len() {
                    self.terminals.remove(i);
                    if i < self.active_terminal {
                        self.active_terminal -= 1;
                    }
                }
                self.active_terminal = self
                    .active_terminal
                    .min(self.terminals.len().saturating_sub(1));
                if self.terminals.is_empty() {
                    self.terminal_open = false;
                }
            }
            Action::SelectTerminal(i) => {
                if i < self.terminals.len() {
                    self.active_terminal = i;
                }
            }
            Action::ToggleSearch => {
                self.search.open = !self.search.open;
                self.search.focus = self.search.open;
                if !self.search.open {
                    self.search.matches.clear();
                    self.search.results.clear();
                    self.search.searched.clear();
                }
            }
            Action::SearchStep(d) => self.search_step(d, ctx),
            Action::CreatePr {
                branch,
                base,
                title,
                body,
                draft,
            } => {
                let Some(forge) = self.forge else { return };
                if self.busy_op.is_some() {
                    return;
                }
                let branch = branch.trim().to_owned();
                let base = base.trim().to_owned();
                let title = title.trim().to_owned();
                if branch.is_empty()
                    || base.is_empty()
                    || title.is_empty()
                    || branch.starts_with('-')
                    || base.starts_with('-')
                {
                    self.status = Some(("Invalid pull request parameters".to_owned(), true));
                    return;
                }
                let remote_name = self
                    .refs
                    .local
                    .iter()
                    .find(|b| b.name == branch && !b.upstream.is_empty() && !b.upstream_gone())
                    .and_then(|b| b.upstream.split_once('/').map(|(r, _)| r.to_owned()))
                    .or_else(|| self.default_remote())
                    .unwrap_or_else(|| "origin".to_owned());
                let remote_url = self
                    .remotes
                    .iter()
                    .find(|r| r.name == remote_name)
                    .map(|r| r.url.clone())
                    .unwrap_or_default();
                let creds = self.credentials.get(&credential_key(&remote_url)).cloned();
                let label = format!("Create {}", forge.noun());
                self.busy_op = Some(label.clone());
                self.op_started = Some(Instant::now());
                self.status = None;
                self.last_op = None;
                self.pending_pr = Some(PendingPr {
                    branch: branch.clone(),
                    base: base.clone(),
                    title: title.clone(),
                    body: body.clone(),
                    draft,
                    remote_url,
                });
                let path = self.path.clone();
                spawn(&self.tx, ctx, move || {
                    // Publish the branch first, so the forge can see it.
                    let pushed = git::run_with(
                        Some(&path),
                        &["push", "-u", &remote_name, &branch],
                        creds.as_ref(),
                    );
                    let result = pushed.and_then(|_| {
                        forge::create(&path, forge, &branch, &base, &title, &body, draft)
                    });
                    Msg::OpDone {
                        label,
                        result,
                        refresh: Refresh::All,
                        kind: OpKind::Normal(Undo::None, String::new()),
                        quiet: false,
                        failed_step: None,
                    }
                });
            }
            Action::OpenUrl(url) => {
                if (url.starts_with("https://") || url.starts_with("http://"))
                    && !url.chars().any(|c| c.is_whitespace() || c.is_control())
                {
                    let _ = open::that_detached(url);
                }
            }
            Action::OpenPath(rel) => {
                if !rel.is_absolute()
                    && !rel
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    let _ = open::that_detached(self.path.join(rel));
                }
            }
            Action::OpenInEditor(rel) => {
                let editor = Settings::get(ctx).editor;
                if let Err(e) = open_in_editor(&editor, &self.path.join(rel), None) {
                    self.status = Some((e, true));
                }
            }
            Action::OpenInEditorAt(rel, line) => {
                let editor = Settings::get(ctx).editor;
                if let Err(e) = open_in_editor(&editor, &self.path.join(rel), Some(line)) {
                    self.status = Some((e, true));
                }
            }
            Action::SetIdentity(name, email) => self.run_ops(
                "Set identity",
                ops(&[
                    &["config", "--local", "user.name", &name],
                    &["config", "--local", "user.email", &email],
                ]),
                Refresh::All,
                plain,
                ctx,
            ),
            Action::SetDiffOptions {
                split,
                ignore_ws,
                whole_file,
            } => {
                // Side by side and the whole file are kept for next time; whitespace is per tab.
                let mut settings = Settings::get(ctx);
                let reload =
                    ignore_ws != self.diff_ignore_ws || whole_file != settings.diff_whole_file;
                settings.diff_split = split;
                settings.diff_whole_file = whole_file;
                settings.set(ctx);
                self.diff_ignore_ws = ignore_ws;
                if reload {
                    self.reopen_diff(ctx);
                }
            }
            Action::FileView { path, rev, tab } => self.open_file_view(path, rev, tab, ctx),
            Action::CloseFileView => self.file_view = None,
            Action::InteractiveRebase { onto, label } => {
                if self.work.in_progress.is_some() {
                    self.status = Some(("Finish the operation in progress before rebasing".to_owned(), true));
                    return;
                }
                self.rebase_seq = self.rebase_seq.wrapping_add(1);
                let seq = self.rebase_seq;
                let path = self.path.clone();
                spawn(&self.tx, ctx, move || {
                    let result = git::rebase_commits(&path, &onto);
                    Msg::RebaseCommits {
                        seq,
                        onto,
                        label,
                        result,
                    }
                });
            }
            Action::Reword { hash, message } => self.reword_commit(hash, message, ctx),
            Action::QuickEdit(hash, edit) => self.quick_edit(hash, edit, ctx),
            Action::EditMessage(hash) => self.start_edit_message(hash, ctx),
            Action::Patch(action) => self.patch_action(action, ctx),
            Action::Ext(action) => self.ext_action(action, ctx),
            Action::FetchWith(args) => {
                self.last_fetch = Instant::now();
                self.run_ops("Fetch", vec![args], Refresh::All, plain, ctx)
            }
            Action::RunRebase => {
                if !self.work.staged.is_empty() || !self.work.conflicted.is_empty() {
                    self.status = Some((
                        "Commit, unstage, or resolve staged/conflicted changes before rebasing"
                            .to_owned(),
                        true,
                    ));
                    return;
                }
                let Some(editor) = &self.rebase else { return };
                if editor.problem().is_some() || self.busy_op.is_some() {
                    return;
                }
                match editor.command() {
                    Ok(command) => {
                        let label = format!("Interactive rebase onto {}", editor.onto_label);
                        self.rebase = None;
                        self.run_undoable(
                            &label,
                            vec![command],
                            Refresh::All,
                            Undo::HeadMove { mode: "--keep" },
                            ctx,
                        );
                    }
                    Err(e) => self.status = Some((e, true)),
                }
            }
            Action::CloseRebase => self.rebase = None,
            Action::SignIn(creds) => {
                if let Some(op) = self.last_op.take() {
                    let key = if op.remote_url.is_empty() {
                        self.default_remote()
                            .and_then(|r| self.remotes.iter().find(|x| x.name == r))
                            .map(|r| credential_key(&r.url))
                            .unwrap_or_else(|| "__default__".to_owned())
                    } else {
                        credential_key(&op.remote_url)
                    };
                    self.credentials.insert(key, creds);
                    self.run_ops_ex(
                        &op.label,
                        op.ops,
                        op.refresh,
                        op.kind,
                        op.quiet,
                        op.autostash,
                        ctx,
                    );
                } else if let Some(pr) = self.pending_pr.take() {
                    let key = if pr.remote_url.is_empty() {
                        self.default_remote()
                            .and_then(|r| self.remotes.iter().find(|x| x.name == r))
                            .map(|r| credential_key(&r.url))
                            .unwrap_or_else(|| "__default__".to_owned())
                    } else {
                        credential_key(&pr.remote_url)
                    };
                    self.credentials.insert(key, creds);
                    self.apply(
                        Action::CreatePr {
                            branch: pr.branch,
                            base: pr.base,
                            title: pr.title,
                            body: pr.body,
                            draft: pr.draft,
                        },
                        ctx,
                    );
                }
            }
            Action::Ignore { pattern, path } => match git::add_to_gitignore(&self.path, &pattern) {
                Err(e) => self.status = Some((e, true)),
                Ok(added) => {
                    if git::is_tracked(&self.path, &path) {
                        self.reload_status(ctx);
                        let is_dir = self.path.join(&path).is_dir();
                        let subject = if is_dir {
                            format!("folder `{path}/` (all tracked files inside it)")
                        } else {
                            format!("`{path}`")
                        };
                        let lit = ext::literal(&path);
                        self.dialog = Some(Dialog::confirm(
                            "Stop tracking in git?",
                            format!(
                                "Added `{pattern}` to .gitignore, but {subject} is still tracked by git.\n\n\
                                 Remove it from git's index while keeping your local file(s) on disk?"
                            ),
                            "Stop tracking",
                            ops(&[&["rm", "-r", "--cached", "--quiet", "--", &lit]]),
                            None,
                        ));
                    } else {
                        let msg = if added {
                            format!("Added {pattern} to .gitignore")
                        } else {
                            format!("{pattern} was already in .gitignore")
                        };
                        self.status = Some((msg, false));
                        self.reload_status(ctx);
                    }
                }
            },
            Action::PickLines { pick, lines } => {
                let Some(DiffView {
                    key: DiffKey::Work(file),
                    data: Some(Ok(data)),
                    ..
                }) = &self.diff
                else {
                    return;
                };
                let Some(patch) = diff::partial_patch(&data.header, &data.lines, &lines, pick)
                else {
                    return;
                };
                let patch = match git::save_patch(&patch) {
                    Ok(p) => p.to_string_lossy().into_owned(),
                    Err(e) => {
                        self.status = Some((e, true));
                        return;
                    }
                };
                let n = lines.len();
                let label = format!(
                    "{} {n} line{} of {}",
                    pick.verb(),
                    if n == 1 { "" } else { "s" },
                    file.path
                );
                let mut args = pick.args().to_vec();
                args.push(&patch);
                if pick == diff::Pick::Discard {
                    // Applying the same patch forward brings the lines back.
                    let undo = Undo::Custom {
                        undo: ops(&[&["apply", &patch]]),
                        redo: ops(&[&args]),
                    };
                    self.run_undoable(&label, ops(&[&args]), Refresh::Status, undo, ctx);
                } else {
                    self.run_ops(&label, ops(&[&args]), Refresh::Status, plain, ctx);
                }
            }
            Action::Compare {
                base,
                base_label,
                target,
                target_label,
                since_fork,
            } => self.compare(
                Compare {
                    base,
                    base_label,
                    target,
                    target_label,
                    since_fork,
                    data: None,
                    request: 0,
                },
                ctx,
            ),
            Action::CompareSinceFork(on) => {
                if let Some(c) = &mut self.compare {
                    c.since_fork = on;
                    self.reload_compare(ctx);
                }
            }
            Action::SwapCompare => {
                // The working tree stays the newer end.
                if let Some(c) = self.compare.take_if(|c| c.target.is_some())
                    && let Some(target) = c.target
                {
                    self.compare(
                        Compare {
                            base: target,
                            base_label: c.target_label,
                            target: Some(c.base),
                            target_label: c.base_label,
                            data: None,
                            ..c
                        },
                        ctx,
                    );
                }
            }
            Action::CloseCompare => {
                self.compare = None;
                if matches!(
                    self.diff,
                    Some(DiffView {
                        key: DiffKey::Range { .. },
                        ..
                    })
                ) {
                    self.clear_diff(ctx);
                }
            }
            Action::ShowLocalChanges => {
                self.file_view = None;
                self.compare = None;
                self.rebase = None;
                self.stash_view = None;
                if let Some(wip_row) = self.index.get(WIP_HASH).copied() {
                    self.show_local_changes = false;
                    self.select(wip_row, ctx);
                    self.scroll_to = Some(wip_row);
                } else {
                    self.show_local_changes = true;
                    self.clear_diff(ctx);
                }
            }
            Action::DismissStatus => {
                self.status = None;
            }
            Action::SearchCode(on) => {
                if self.search.code != on {
                    self.search.code = on;
                    self.run_search(ctx);
                }
                self.search.focus = true;
            }
            Action::SaveConflict => {
                if let Some(editor) = &self.conflict {
                    match editor.save(&self.path) {
                        Ok(()) => {
                            let file = editor.path.clone();
                            self.run_ops(
                                "Mark resolved",
                                ops(&[&["add", "--", &file]]),
                                Refresh::Status,
                                plain,
                                ctx,
                            );
                        }
                        Err(e) => self.status = Some((e, true)),
                    }
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        let t = Tokens::get(&ctx);
        let mut acts: Vec<Action> = std::mem::take(&mut self.queued);

        let popup_open = egui::Popup::is_any_open(&ctx);
        let free_keys = !self.has_dialog()
            && !self.keys_blocked
            && !popup_open
            && !ctx.egui_wants_keyboard_input();
        if free_keys {
            let (esc, undo, redo, find) = ctx.input(|i| {
                (
                    i.key_pressed(Key::Escape),
                    i.modifiers.command && !i.modifiers.shift && i.key_pressed(Key::Z),
                    i.modifiers.command
                        && (i.key_pressed(Key::Y) || (i.modifiers.shift && i.key_pressed(Key::Z))),
                    i.modifiers.command && i.key_pressed(Key::F),
                )
            });
            let esc = esc && !self.was_typing && self.reword.is_none();
            if esc && self.diff.is_some() {
                acts.push(Action::CloseDiff);
            } else if esc && self.rebase.is_some() {
                acts.push(Action::CloseRebase);
            } else if esc && self.file_view.is_some() {
                acts.push(Action::CloseFileView);
            } else if esc && self.compare.is_some() {
                acts.push(Action::CloseCompare);
            } else if esc && self.search.open {
                acts.push(Action::ToggleSearch);
            }
            if undo {
                acts.push(Action::Undo);
            }
            if redo {
                acts.push(Action::Redo);
            }
            if find {
                self.search.open = true;
                self.search.focus = true;
            }
        } else if self.search.open
            && !self.has_dialog()
            && !self.keys_blocked
            && !popup_open
            && ctx.input(|i| i.modifiers.command && i.key_pressed(Key::F))
        {
            self.search.focus = true;
        }

        egui::Panel::top("repo_toolbar")
            .frame(
                egui::Frame::new()
                    .fill(t.chrome)
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .stroke(egui::Stroke::new(1.0, t.separator)),
            )
            .show(ui, |ui| self.toolbar(ui, &mut acts));
        self.bisect_bar(ui, &mut acts);
        let side = egui::Frame::new().fill(t.card);
        egui::Panel::left("repo_branches")
            .resizable(true)
            .default_size(270.0)
            .size_range(170.0..=520.0)
            .frame(side)
            .show(ui, |ui| self.sidebar(ui, &mut acts));
        egui::Panel::right("repo_detail")
            .resizable(true)
            .default_size(420.0)
            .size_range(280.0..=900.0)
            .frame(side)
            .show(ui, |ui| self.detail_panel(ui, &mut acts));
        if self.terminal_open {
            egui::Panel::bottom("repo_terminal")
                .resizable(true)
                .default_size(260.0)
                .size_range(90.0..=900.0)
                .frame(egui::Frame::new().fill(t.card))
                .show(ui, |ui| self.terminal_panel(ui, &mut acts));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(t.dock))
            .show(ui, |ui| {
                if self.conflict.is_some() {
                    self.conflict_view(ui, &mut acts);
                } else if self.rebase.is_some() {
                    self.rebase_view(ui, &mut acts);
                } else if self.diff.is_some() {
                    self.diff_view(ui, &mut acts);
                } else if self.file_view.is_some() {
                    self.file_view(ui, &mut acts);
                } else {
                    self.graph_view(ui, &mut acts);
                }
            });

        self.dialogs(&ctx, &mut acts);
        for action in acts {
            self.apply(action, &ctx);
        }
        self.was_typing = ctx.egui_wants_keyboard_input();
    }

    fn toolbar(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        if self.prompt.is_some() {
            return self.prompt_bar(ui, acts);
        }
        let t = Tokens::get(ui.ctx());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let idle = self.busy_op.is_none();

            let undo_tip = match self.undo_stack.last() {
                Some(e) => format!("Undo {} (Ctrl+Z)", e.label),
                None => "Nothing to undo".to_owned(),
            };
            if icons::tool_button(ui, "undo-2", "", idle && !self.undo_stack.is_empty())
                .on_hover_text(undo_tip)
                .on_disabled_hover_text("Nothing to undo")
                .clicked()
            {
                acts.push(Action::Undo);
            }
            let redo_tip = match self.redo_stack.last() {
                Some(e) => format!("Redo {} (Ctrl+Y)", e.label),
                None => "Nothing to redo".to_owned(),
            };
            if icons::tool_button(ui, "redo-2", "", idle && !self.redo_stack.is_empty())
                .on_hover_text(redo_tip)
                .on_disabled_hover_text("Nothing to redo")
                .clicked()
            {
                acts.push(Action::Redo);
            }
            widgets::toolbar_separator(ui);

            let has_remote = !self.remotes.is_empty();
            let fetched = format!(
                "Fetch… (git fetch)\nLast fetched {} ago",
                short_duration(self.last_fetch.elapsed())
            );
            if icons::tool_button(ui, "cloud-download", "Fetch", idle && has_remote)
                .on_hover_text(fetched)
                .clicked()
            {
                acts.push(Action::Dialog(self.sync_dialog(sync::SyncKind::Fetch)));
            }
            let (ahead, behind) = self
                .refs
                .current
                .as_ref()
                .and_then(|c| self.refs.local.iter().find(|b| &b.name == c))
                .map(|b| b.ahead_behind())
                .unwrap_or((0, 0));
            if icons::tool_button_count(
                ui,
                "arrow-down-to-line",
                "Pull",
                behind,
                idle && has_remote,
            )
            .on_hover_text("Pull… (merge, rebase or fast-forward the current branch)")
            .clicked()
            {
                acts.push(Action::Dialog(self.sync_dialog(sync::SyncKind::Pull)));
            }
            if icons::tool_button_count(ui, "arrow-up-from-line", "Push", ahead, idle && has_remote)
                .on_hover_text("Push… (also publishes a branch that has no upstream yet)")
                .clicked()
            {
                acts.push(Action::Dialog(self.sync_dialog(sync::SyncKind::Push)));
            }
            widgets::toolbar_separator(ui);
            let head = self.refs.head.clone();
            if icons::tool_button(ui, "git-branch-plus", "Branch", idle && !head.is_empty())
                .on_hover_text("Create a branch at HEAD")
                .clicked()
            {
                acts.push(Action::Dialog(Dialog::create_branch(&head, "HEAD")));
            }
            if icons::tool_button(ui, "archive", "Stash", idle && !self.work.is_clean())
                .on_hover_text("Stash all changes, untracked files included")
                .on_disabled_hover_text("No changes to stash")
                .clicked()
            {
                acts.push(Action::Dialog(self.stash_dialog()));
            }
            let terminal = icons::tool_button(ui, "square-terminal", "Terminal", true)
                .on_hover_text("Terminal in the repository folder");
            if self.terminal_open {
                widgets::active_underline(ui, terminal.rect, t.accent);
            }
            if terminal.clicked() {
                acts.push(Action::ToggleTerminal);
            }
            let search =
                icons::tool_button(ui, "search", "", true).on_hover_text("Search commits (Ctrl+F)");
            if self.search.open {
                widgets::active_underline(ui, search.rect, t.accent);
            }
            if search.clicked() {
                acts.push(Action::ToggleSearch);
            }
            let open =
                icons::tool_button(ui, "external-link", "", true).on_hover_text("Open the folder…");
            egui::Popup::menu(&open)
                .style(crate::theme::menu_style)
                .show(|ui| {
                    if ui.button(reveal_label()).clicked() {
                        acts.push(Action::OpenPath(PathBuf::new()));
                    }
                    if ui.button("Open in editor").clicked() {
                        acts.push(Action::OpenInEditor(PathBuf::new()));
                    }
                    if ui.button("Copy path").clicked() {
                        ui.ctx().copy_text(self.path.display().to_string());
                    }
                });
            let tools = icons::tool_button(ui, "wrench", "", true).on_hover_text("Tools");
            egui::Popup::menu(&tools)
                .style(crate::theme::menu_style)
                .show(|ui| self.tools_menu(ui, acts));
            widgets::toolbar_separator(ui);
            if icons::tool_button(ui, "refresh-cw", "", !self.loading)
                .on_hover_text("Reload (F5)")
                .clicked()
            {
                acts.push(Action::Reload);
            }
            widgets::toolbar_separator(ui);

            match &self.refs.current {
                Some(branch) => {
                    icons::inline(ui, "git-branch", 14.0, t.text_dim);
                    ui.label(egui::RichText::new(branch).font(crate::theme::medium(12.5)));
                }
                None if self.refs.head.is_empty() => {
                    ui.label(egui::RichText::new("No commits yet").color(t.text_dim));
                }
                None => {
                    ui.label(
                        egui::RichText::new(format!(
                            "Detached HEAD at {}",
                            self.refs.head.get(..8).unwrap_or(&self.refs.head)
                        ))
                        .color(t.warning),
                    );
                }
            }

            // Branches left out of the graph: how many, and a way back to all of them.
            if self.graph_filter.is_active() {
                ui.add_space(10.0);
                let f = &self.graph_filter;
                let text = if f.solo.is_empty() {
                    let n = f.hidden.len();
                    format!("{n} branch{} hidden", if n == 1 { "" } else { "es" })
                } else {
                    let n = f.solo.len();
                    format!("{n} branch{} soloed", if n == 1 { "" } else { "es" })
                };
                if icons::tool_button(ui, "eye-off", &text, true)
                    .on_hover_text("Show every branch in the graph again")
                    .clicked()
                {
                    acts.push(Action::BranchFilter(branch_filter::Change::Clear));
                }
            }

            if let Some(state) = self.work.in_progress {
                ui.add_space(10.0);
                widgets::chip(ui, &format!("{} in progress", state.label()), t.warning);
                if ui
                    .add_enabled(
                        idle,
                        egui::Button::new(format!("Abort {}", state.label().to_lowercase())),
                    )
                    .clicked()
                {
                    acts.push(abort_action(state));
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some((msg, is_err)) = &self.status {
                    if icons::small_button(ui, "x", "Dismiss").clicked() {
                        acts.push(Action::DismissStatus);
                    }
                    let color = if *is_err { t.danger } else { t.text_dim };
                    let first_line = headline(msg);
                    ui.add(
                        egui::Label::new(egui::RichText::new(first_line).color(color)).truncate(),
                    )
                    .on_hover_text(format!("{msg}\n\nClick × to dismiss"));
                }
                if let Some(op) = &self.busy_op {
                    ui.label(egui::RichText::new(format!("{op}…")).color(t.text_dim));
                    ui.spinner();
                } else if self.loading {
                    ui.spinner();
                }
            });
        });
    }

    /// The bottom panel: terminal tabs and the active shell.
    fn terminal_panel(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let t = Tokens::get(ui.ctx());
        widgets::terminal_tabs(ui, &self.terminals, self.active_terminal, acts);
        if let Some(err) = &self.terminal_error {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.colored_label(t.danger, err);
            });
            if self.terminals.is_empty() {
                return;
            }
        }
        let i = self.active_terminal;
        let Some(term) = self.terminals.get_mut(i) else {
            return;
        };
        if term.has_exited() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("The shell has exited.").color(t.text_dim));
                if ui.button("Close").clicked() {
                    acts.push(Action::CloseTerminal(i));
                }
            });
            return;
        }
        term.ui(ui);
    }

    /// What the command palette offers for this repository.
    pub(crate) fn palette_items(&self) -> Vec<(String, String, Action)> {
        let mut items: Vec<(String, String, Action)> = vec![
            ("Local changes".into(), "view".into(), Action::ShowLocalChanges),
            ("Fetch".into(), "remote".into(), Action::Fetch),
            ("Pull".into(), "remote".into(), Action::Pull),
            ("Push".into(), "remote".into(), Action::Push),
            (
                "Apply patch…".into(),
                "patch".into(),
                Action::Dialog(Dialog::apply_patch(&self.path)),
            ),
            ("Undo".into(), "Ctrl+Z".into(), Action::Undo),
            ("Redo".into(), "Ctrl+Y".into(), Action::Redo),
            (
                "Stash changes".into(),
                "stash".into(),
                Action::Dialog(self.stash_dialog()),
            ),
            (
                "Search commits".into(),
                "Ctrl+F".into(),
                Action::ToggleSearch,
            ),
            (
                "Toggle terminal".into(),
                "terminal".into(),
                Action::ToggleTerminal,
            ),
            (
                "New terminal".into(),
                "terminal".into(),
                Action::NewTerminal {
                    shell: None,
                    cwd: None,
                },
            ),
            ("Reload".into(), "F5".into(), Action::Reload),
            (
                reveal_label().into(),
                "folder".into(),
                Action::OpenPath(PathBuf::new()),
            ),
            (
                "Open in editor".into(),
                "folder".into(),
                Action::OpenInEditor(PathBuf::new()),
            ),
        ];
        if !self.refs.head.is_empty() {
            items.push((
                "Create branch".into(),
                "branch".into(),
                Action::Dialog(Dialog::create_branch(&self.refs.head, "HEAD")),
            ));
            items.push((
                "Create tag".into(),
                "tag".into(),
                Action::Dialog(Dialog::tag(&self.refs.head, "HEAD", self.default_remote())),
            ));
        }
        for b in &self.refs.local {
            if !b.is_head {
                items.push((
                    format!("Check out {}", b.name),
                    "branch".into(),
                    Action::Checkout(b.name.clone()),
                ));
            }
        }
        if let Some(current) = &self.refs.current {
            for b in &self.refs.local {
                if !b.is_head {
                    items.push((
                        format!("Changes {} adds to {current}", b.name),
                        "compare".into(),
                        compare_action(current, &b.name, true),
                    ));
                    items.push((
                        format!("Compare {current} with {}", b.name),
                        "compare".into(),
                        compare_action(current, &b.name, false),
                    ));
                    items.push((
                        format!("Merge {} into {current}", b.name),
                        "merge".into(),
                        merge_action(&b.name, current),
                    ));
                    items.push((
                        format!("Rebase {current} onto {}", b.name),
                        "rebase".into(),
                        rebase_action(&b.name, current),
                    ));
                }
            }
        }
        for (remote, branches) in &self.refs.remotes {
            for b in branches {
                items.push((
                    format!("Check out {}", b.name),
                    remote.clone(),
                    Action::CheckoutRemote(b.name.clone()),
                ));
            }
        }
        items
    }
}

/// Compares branch (or commit) `base` with `target`; `since_fork`: only what `target` changed
/// since they diverged.
pub(crate) fn compare_action(base: &str, target: &str, since_fork: bool) -> Action {
    Action::Compare {
        base: base.to_owned(),
        base_label: base.to_owned(),
        target: Some(target.to_owned()),
        target_label: target.to_owned(),
        since_fork,
    }
}

pub(crate) fn merge_action(branch: &str, current: &str) -> Action {
    Action::Dialog(branch_ops::merge_dialog(branch, current))
}

/// Moves the current branch forward to `branch`; fails (and says so) if they have diverged.
pub(crate) fn fast_forward_action(branch: &str, current: &str) -> Action {
    Action::UndoableOps {
        label: format!("Fast-forward {current} to {branch}"),
        ops: ops(&[&["merge", "--ff-only", branch]]),
        refresh: Refresh::All,
        undo: Undo::HeadMove { mode: "--keep" },
    }
}

/// Fast-forwards `b` to its remote-tracking branch as it is now: nothing is fetched from the
/// remote, checked out or stashed. The checked-out branch by `merge --ff-only`; any other by a
/// fetch from this repository into it, which git refuses unless it is a fast-forward (and
/// while the branch is checked out in another worktree). `None` when `b` tracks nothing.
pub(crate) fn fast_forward_upstream_action(b: &git::Branch) -> Option<Action> {
    if b.upstream.is_empty() || b.upstream_ref.is_empty() || b.upstream_gone() {
        return None;
    }
    let label = format!("Fast-forward {} to {}", b.name, b.upstream);
    if b.is_head {
        return Some(Action::UndoableOps {
            label,
            ops: ops(&[&["merge", "--ff-only", &b.upstream_ref]]),
            refresh: Refresh::All,
            undo: Undo::HeadMove { mode: "--keep" },
        });
    }
    // Full ref names, and no pruning, tags or submodules: with `fetch.prune` set, a fetch from
    // `.` would otherwise delete the branch it is meant to move.
    let run = ops(&[&[
        "fetch",
        "--no-prune",
        "--no-tags",
        "--no-recurse-submodules",
        ".",
        &format!("{}:refs/heads/{}", b.upstream_ref, b.name),
    ]]);
    Some(Action::UndoableOps {
        label,
        ops: run.clone(),
        refresh: Refresh::All,
        undo: Undo::Custom {
            undo: ops(&[&["branch", "-f", &b.name, &b.target]]),
            redo: run,
        },
    })
}

pub(crate) fn rebase_action(onto: &str, current: &str) -> Action {
    Action::UndoableOps {
        label: format!("Rebase {current} onto {onto}"),
        ops: ops(&[&["rebase", onto]]),
        refresh: Refresh::All,
        undo: Undo::HeadMove { mode: "--keep" },
    }
}

fn abort_action(state: InProgress) -> Action {
    Action::Ops {
        label: format!("Abort {}", state.label().to_lowercase()),
        ops: ops(&[&[state.command(), "--abort"]]),
        refresh: Refresh::All,
    }
}

pub(crate) fn reveal_label() -> &'static str {
    if cfg!(windows) {
        "Show in Explorer"
    } else if cfg!(target_os = "macos") {
        "Show in Finder"
    } else {
        "Open folder"
    }
}

/// Runs the user's editor command with `path`, at `line` when the editor is one that can be
/// told (through `cmd` on Windows, where editors are usually `.cmd` scripts), and reaps the
/// child in a background thread.
pub(crate) fn open_in_editor(editor: &str, path: &Path, line: Option<u32>) -> Result<(), String> {
    let editor = if editor.trim().is_empty() {
        "code"
    } else {
        editor.trim()
    };
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/D", "/C", editor])
            .args(editor_args(editor, path, line));
        c
    } else {
        let mut c = std::process::Command::new(editor);
        c.args(editor_args(editor, path, line));
        crate::shell_path::apply(&mut c);
        c
    };
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Could not run {editor}: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn credential_scope(url: &str) -> String {
    let u = url.trim();
    if u.is_empty() {
        return String::new();
    }
    if let Some((scheme, rest)) = u.split_once("://") {
        if scheme.eq_ignore_ascii_case("file") {
            return String::new();
        }
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        if !host.is_empty() {
            return format!(
                "{}://{}",
                scheme.to_ascii_lowercase(),
                host.to_ascii_lowercase()
            );
        }
        return String::new();
    }
    let bytes = u.as_bytes();
    if !(bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        && let Some((left, _)) = u.split_once(':')
    {
        let host = left.rsplit_once('@').map_or(left, |(_, h)| h);
        if !host.is_empty() {
            return host.to_ascii_lowercase();
        }
    }
    String::new()
}

fn credential_key(url: &str) -> String {
    let scope = credential_scope(url);
    if scope.is_empty() {
        "__default__".to_owned()
    } else {
        scope
    }
}

/// The arguments that open `path` at `line` in `editor`, by the editor's family; just the path
/// for an editor that isn't known.
fn editor_args(editor: &str, path: &Path, line: Option<u32>) -> Vec<std::ffi::OsString> {
    let Some(line) = line else {
        return vec![path.into()];
    };
    let program = editor.split_whitespace().next().unwrap_or(editor);
    let name = Path::new(program)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let at = |sep: &str| {
        let mut s = path.as_os_str().to_owned();
        s.push(format!("{sep}{line}"));
        s
    };
    match name.as_str() {
        "code" | "code-insiders" | "codium" | "vscodium" | "cursor" | "windsurf" => {
            vec!["-g".into(), at(":")]
        }
        "subl" | "sublime_text" | "zed" | "zeditor" | "atom" | "pulsar" | "lapce" => vec![at(":")],
        "idea" | "idea64" | "pycharm" | "pycharm64" | "webstorm" | "webstorm64" | "phpstorm"
        | "phpstorm64" | "rider" | "rider64" | "clion" | "clion64" | "goland" | "goland64"
        | "rubymine" | "rubymine64" | "rustrover" | "rustrover64" | "studio" | "studio64" => {
            vec!["--line".into(), line.to_string().into(), path.into()]
        }
        "vim" | "nvim" | "gvim" | "mvim" | "emacs" | "emacsclient" | "nano" | "micro" | "kak" => {
            vec![format!("+{line}").into(), path.into()]
        }
        _ => vec![path.into()],
    }
}

fn short_duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        _ => format!("{} h", s / 3600),
    }
}

impl RepoTab {
    /// A question in place of the toolbar: what happened, the choices, and a close button
    /// (Esc) that puts the toolbar back.
    fn prompt_bar(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let t = Tokens::get(ui.ctx());
        let mut chosen = None;
        let mut close = ui.input(|i| i.key_pressed(egui::Key::Escape)) && !self.keys_blocked;
        if let Some(prompt) = &self.prompt {
            ui.horizontal(|ui| {
                ui.set_min_height(ui.spacing().interact_size.y + 4.0);
                ui.add_space(4.0);
                icons::inline(ui, "circle-alert", 16.0, t.warning);
                ui.label(egui::RichText::new(&prompt.text).color(t.text));
                ui.add_space(6.0);
                let idle = self.busy_op.is_none();
                for (i, (label, tip, _, danger)) in prompt.options.iter().enumerate() {
                    let text = if *danger {
                        egui::RichText::new(label).color(t.danger)
                    } else if i == 0 {
                        egui::RichText::new(label).color(t.accent_text)
                    } else {
                        egui::RichText::new(label)
                    };
                    let mut button = egui::Button::new(text);
                    if i == 0 && !*danger {
                        button = button.fill(t.accent);
                    }
                    if ui
                        .add_enabled(idle, button)
                        .on_hover_text(tip.as_str())
                        .clicked()
                    {
                        chosen = Some(i);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::small_button(ui, "x", "Close (Esc)").clicked() {
                        close = true;
                    }
                });
            });
        }
        if let Some(i) = chosen
            && let Some(prompt) = self.prompt.take()
            && let Some((_, _, action, _)) = prompt.options.into_iter().nth(i)
        {
            acts.push(action);
        } else if close {
            self.prompt = None;
        }
    }
}

/// The line of a git error worth showing in the toolbar: conflicts and `error:`/`fatal:` lines
/// beat progress chatter like "Auto-merging x".
fn headline(msg: &str) -> String {
    let (label, body) = msg.split_once(": ").unwrap_or(("", msg));
    let conflicts = body.lines().filter(|l| l.starts_with("CONFLICT")).count();
    if conflicts > 0 {
        let files = if conflicts == 1 { "file" } else { "files" };
        return format!("{label}: conflicts in {conflicts} {files}, resolve them in Changes");
    }
    let line = body
        .lines()
        .find(|l| l.starts_with("error:") || l.starts_with("fatal:"))
        .or_else(|| body.lines().find(|l| !l.trim().is_empty()))
        .unwrap_or_default();
    if label.is_empty() {
        line.to_owned()
    } else {
        format!("{label}: {line}")
    }
}

/// A hash as the UI shows it: its first 8 characters.
pub(crate) fn short(hash: &str) -> &str {
    hash.get(..8).unwrap_or(hash)
}

/// `&[&["checkout", "main"]]` → owned argument lists.
pub(crate) fn ops(list: &[&[&str]]) -> Vec<Vec<String>> {
    list.iter()
        .map(|args| args.iter().map(|a| (*a).to_owned()).collect())
        .collect()
}

/// Runs `job` on a background thread and wakes the UI when its message is sent.
fn spawn(tx: &Sender<Msg>, ctx: &egui::Context, job: impl FnOnce() -> Msg + Send + 'static) {
    let tx = tx.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(job());
        ctx.request_repaint();
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn editors_open_at_a_line_their_own_way() {
        use std::ffi::OsString;
        let p = std::path::Path::new("src/a.rs");
        let args = |e: &str, l| super::editor_args(e, p, l);
        let os = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(args("code", Some(12)), os(&["-g", "src/a.rs:12"]));
        assert_eq!(
            args("C:/Tools/Cursor.exe", Some(3)),
            os(&["-g", "src/a.rs:3"])
        );
        assert_eq!(args("subl", Some(4)), os(&["src/a.rs:4"]));
        assert_eq!(args("idea64", Some(5)), os(&["--line", "5", "src/a.rs"]));
        assert_eq!(args("nvim", Some(6)), os(&["+6", "src/a.rs"]));
        assert_eq!(args("notepad++", Some(7)), os(&["src/a.rs"]));
        assert_eq!(args("code", None), os(&["src/a.rs"]));
    }

    use super::{DiffKey, DiffView, headline};

    #[test]
    fn an_older_diff_answer_does_not_replace_a_newer_one() {
        let key = DiffKey::Commit {
            hash: "abc".to_owned(),
            path: "a.rs".to_owned(),
        };
        let mut view = DiffView::new(key.clone());
        let older = view.next_request();
        let newer = view.next_request();
        view.arrived(newer, Err("newer".to_owned()));
        view.arrived(older, Err("older".to_owned()));
        assert!(matches!(&view.data, Some(Err(e)) if e == "newer"));
        // Nor an answer to a diff opened before, of the same file.
        let mut reopened = DiffView::new(key);
        let request = reopened.next_request();
        reopened.arrived(newer, Err("closed".to_owned()));
        assert!(reopened.data.is_none());
        reopened.arrived(request, Err("open".to_owned()));
        assert!(matches!(&reopened.data, Some(Err(e)) if e == "open"));
    }

    #[test]
    fn headline_prefers_conflicts_and_errors() {
        let merge = "Merge x failed: Auto-merging a.rs
CONFLICT (content): Merge conflict in a.rs
Automatic merge failed";
        assert_eq!(
            headline(merge),
            "Merge x failed: conflicts in 1 file, resolve them in Changes"
        );
        let push = "Push failed: To origin
 ! [rejected] main
error: failed to push some refs";
        assert_eq!(
            headline(push),
            "Push failed: error: failed to push some refs"
        );
        assert_eq!(headline("Fetch: done"), "Fetch: done");
    }

    #[test]
    fn ignore_tracked_file_uses_literal_pathspec_and_select_clears_overlays() {
        use super::test_support::*;
        use super::{Action, Dialog};

        let dir = repo("mod-audit-fixes");
        write(&dir, "glob[1].txt", "hello\n");
        git(&dir, &["add", "--", ":(literal)glob[1].txt"]);
        git(&dir, &["commit", "-q", "-m", "add glob file"]);
        let head = git(&dir, &["rev-parse", "HEAD"]);
        let mut f = Fixture::open(&dir);

        // Action::Ignore on a tracked glob-like path must pass :(literal) to `git rm --cached`.
        f.tab.apply(
            Action::Ignore {
                pattern: "glob[1].txt".to_owned(),
                path: "glob[1].txt".to_owned(),
            },
            &f.ctx,
        );
        match &f.tab.dialog {
            Some(Dialog::Confirm { ops, .. }) => {
                assert_eq!(
                    ops.first().and_then(|v| v.last()).map(String::as_str),
                    Some(":(literal)glob[1].txt")
                );
            }
            _ => unreachable!("expected stop-tracking confirm dialog"),
        }
        f.tab.dialog = None;

        // Re-selecting the same commit clears Compare/FileView/Rebase overlays.
        f.tab.select_hash(&head, &f.ctx);
        f.pump();
        f.run(Action::Compare {
            base: head.clone(),
            base_label: "HEAD".to_owned(),
            target: None,
            target_label: "working tree".to_owned(),
            since_fork: false,
        });
        assert!(f.tab.compare.is_some());
        f.tab.select_hash(&head, &f.ctx);
        assert!(f.tab.compare.is_none());
    }

    #[test]
    fn amend_is_ignored_during_merge_and_esc_while_typing_keeps_conflict_open() {
        use super::test_support::*;
        use super::{Action, DiffKey};

        let dir = repo("mod-amend-esc");
        write(&dir, "f.txt", "base\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "base"]);
        git(&dir, &["checkout", "-q", "-b", "side"]);
        write(&dir, "f.txt", "side\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "side"]);
        git(&dir, &["checkout", "-q", "main"]);
        write(&dir, "f.txt", "main\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "main"]);
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["merge", "--no-commit", "side"])
            .output();

        let mut f = Fixture::open(&dir);
        assert!(f.tab.work.in_progress.is_some());
        // Simulate amend previously checked before merge started.
        f.tab.amend = true;
        f.tab.commit_summary = "Merge side".to_owned();
        // Open ConflictEditor, focus its RESULT TextEdit, press Esc, and verify ConflictEditor stays open.
        if let Some(cf) = f.tab.work.conflicted.first().cloned() {
            use egui_kittest::kittest::Queryable;

            f.tab.apply(Action::OpenDiff(DiffKey::Work(cf)), &f.ctx);
            assert!(f.tab.conflict.is_some());
            let mut harness = egui_kittest::Harness::builder()
                .with_size([900.0, 600.0])
                .build_ui(|ui| {
                    if ui.ctx().cumulative_frame_nr() == 0 {
                        crate::theme::install_fonts(ui.ctx());
                        ui.ctx().request_repaint();
                        return;
                    }
                    f.tab.ui(ui);
                });
            harness.run();
            let result_box = harness
                .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
                .next();
            assert!(result_box.is_some(), "expected ConflictEditor RESULT TextEdit");
            if let Some(node) = result_box {
                node.click();
            }
            harness.run();
            harness.key_press(egui::Key::Escape);
            harness.run();
            drop(harness);
            assert!(
                f.tab.conflict.is_some(),
                "Esc while unfocusing a TextEdit must not discard ConflictEditor"
            );
            assert!(
                !f.tab.amend,
                "amend flag must clear while a merge/rebase is in progress"
            );
        }
    }

    #[test]
    fn show_local_changes_and_dismiss_status() {
        use super::test_support::*;
        use super::{Action, WIP_HASH};

        let dir = repo("mod-local-changes");
        let head = git(&dir, &["rev-parse", "HEAD"]);
        let mut f = Fixture::open(&dir);

        // Clean repo: ShowLocalChanges turns on the Changes view; selecting HEAD turns it off.
        assert!(!f.tab.showing_local_changes());
        f.tab.apply(Action::ShowLocalChanges, &f.ctx);
        assert!(f.tab.showing_local_changes());
        f.tab.apply(Action::SelectHash(head), &f.ctx);
        assert!(!f.tab.showing_local_changes());

        // Dirty repo: ShowLocalChanges selects the WIP row.
        write(&dir, "dirty.txt", "changed\n");
        f.reload();
        f.tab.apply(Action::ShowLocalChanges, &f.ctx);
        assert!(f.tab.showing_local_changes());
        assert_eq!(
            f.tab.selected.and_then(|r| f.tab.commits.get(r)).map(|c| c.hash.as_str()),
            Some(WIP_HASH)
        );

        // DismissStatus clears toolbar errors/status.
        f.tab.status = Some(("Fast-forward failed".to_owned(), true));
        f.tab.apply(Action::DismissStatus, &f.ctx);
        assert!(f.tab.status.is_none());
    }

    #[test]
    fn a_message_pasted_into_the_commit_box_fills_summary_and_description() {
        use super::test_support::*;
        use egui_kittest::kittest::Queryable;

        let dir = repo("commit-box-paste");
        write(&dir, "a.txt", "two\n");
        let mut f = Fixture::open(&dir);
        let mut harness = egui_kittest::Harness::builder()
            .with_size([1200.0, 800.0])
            .build_ui(|ui| {
                if ui.ctx().cumulative_frame_nr() == 0 {
                    crate::theme::install_fonts(ui.ctx());
                    ui.ctx().request_repaint();
                    return;
                }
                f.tab.ui(ui);
            });
        harness.run();
        assert!(harness.query_by_label("Skip hooks").is_some());
        // The commit summary: the lowest one-line field (the branch filter is at the top).
        let summary = harness
            .get_all_by_role(egui::accesskit::Role::TextInput)
            .max_by(|a, b| a.rect().top().total_cmp(&b.rect().top()));
        assert!(summary.is_some(), "no one-line field");
        if let Some(summary) = summary {
            summary.click();
        }
        harness.run();
        harness.event(egui::Event::Paste(
            "Fix the login\n\nThe token expired too early.\n".to_owned(),
        ));
        harness.run();
        drop(harness);
        assert_eq!(f.tab.commit_summary, "Fix the login");
        assert_eq!(f.tab.commit_body, "The token expired too early.");
    }

    #[cfg(unix)]
    #[test]
    fn skip_hooks_commits_past_a_failing_pre_commit_hook() {
        use super::Action;
        use super::test_support::*;
        use std::os::unix::fs::PermissionsExt;

        let dir = repo("commit-no-verify");
        let hook = dir.join(".git/hooks/pre-commit");
        std::fs::create_dir_all(dir.join(".git/hooks")).unwrap_or_default();
        std::fs::write(&hook, "#!/bin/sh\necho blocked by hook >&2\nexit 1\n").unwrap_or_default();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap_or_default();
        let mut f = Fixture::open(&dir);

        // The hook stops a plain commit, and the message stays to retry with.
        write(&dir, "a.txt", "two\n");
        f.reload();
        f.tab.commit_summary = "Past the hook".to_owned();
        f.run(Action::Commit);
        assert!(f.failed(), "{:?}", f.status());
        assert_eq!(git(&dir, &["log", "-1", "--format=%s"]), "initial");
        assert_eq!(f.tab.commit_summary, "Past the hook");

        f.tab.no_verify = true;
        f.run(Action::Commit);
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(git(&dir, &["log", "-1", "--format=%s"]), "Past the hook");
        assert!(!f.tab.no_verify, "skipping hooks is for one commit");

        // Amending skips them the same way.
        f.tab.amend = true;
        f.tab.no_verify = true;
        f.tab.commit_summary = "Amended past the hook".to_owned();
        f.run(Action::Commit);
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&dir, &["log", "--format=%s"]),
            "Amended past the hook\ninitial"
        );
    }
}
