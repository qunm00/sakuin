//! Filesystem watcher for live index synchronisation.
//!
//! [`FileWatcher`] wraps a `notify` debounced watcher running in a
//! background thread. Raw filesystem events are coalesced by the debouncer,
//! filtered down to relevant paths, translated into [`WatchEvent`]s carrying
//! *relative* paths, and queued on a channel that callers drain via
//! [`FileWatcher::poll`].
//!
//! [`FileWatcher`] makes no filesystem calls: file-vs-directory decisions
//! (e.g. "rescan this subtree" vs "re-index this single file") are left to
//! the consumer, [`crate::indexer::Indexer`].

use notify::event::{EventKind, ModifyKind, RenameMode};
use notify::{RecursiveMode};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, DebouncedEvent};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Debounce window for coalescing rapid filesystem events (e.g. an editor
/// saving a file several times in quick succession).
pub const DEBOUNCE_INTERVAL: Duration = Duration::from_millis(200);

/// A single filesystem change relevant to the index.
///
/// All paths are relative to the workspace root, matching how
/// [`Scanner`](crate::scanner::Scanner) reports files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// A file was created or modified.
    Changed(PathBuf),
    /// A file (or directory) was renamed; carries the old and new relative
    /// paths.
    Renamed(PathBuf, PathBuf),
    /// A file (or directory) was removed.
    Removed(PathBuf),
}

/// Watches a directory tree and reports relevant file changes.
///
/// The watcher runs on a background thread; the caller keeps this struct
/// alive and drains pending events with [`FileWatcher::poll`].
pub struct FileWatcher {
    root: PathBuf,
    receiver: Option<Receiver<Vec<WatchEvent>>>,
    stop_sender: Option<Sender<()>>,
    join_handle: Option<JoinHandle<()>>,
}

impl FileWatcher {
    /// Create a watcher for the directory rooted at `root` (not yet
    /// started). The root is canonicalised so that event paths can be
    /// reliably converted to relative paths.
    pub fn new(root: &Path) -> Self {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_owned());
        Self {
            root,
            receiver: None,
            stop_sender: None,
            join_handle: None,
        }
    }

    /// Start watching the root recursively. Idempotent: calling twice is a
    /// no-op. Returns an error if the watcher could not be initialised (for
    /// example when the root cannot be watched).
    pub fn watch(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.join_handle.is_some() {
            return Ok(());
        }

        let root = self.root.clone();
        let (event_sender, event_receiver) = mpsc::channel();
        let (stop_sender, stop_receiver) = mpsc::channel::<()>();
        let (setup_sender, setup_receiver) = mpsc::channel::<Result<(), String>>();

        let handle = thread::Builder::new()
            .name("sakuin-watcher".to_string())
            .spawn(move || {
                run_watcher_thread(&root, event_sender, stop_receiver, setup_sender);
            })?;

        // Wait for the thread to finish setting up so that initialisation
        // errors surface from `watch()` itself rather than asynchronously.
        match setup_receiver.recv() {
            Ok(Ok(())) => {
                self.receiver = Some(event_receiver);
                self.stop_sender = Some(stop_sender);
                self.join_handle = Some(handle);
                Ok(())
            }
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(error.into())
            }
            Err(error) => Err(Box::new(error)),
        }
    }

    /// Drain all pending events.
    ///
    /// Returns the events accumulated since the last call, in the order they
    /// were produced, and `Err(WatcherStopped)` once the background thread
    /// has exited (its event channel closed).
    pub fn poll(&self) -> Result<Vec<WatchEvent>, WatcherStopped> {
        let mut collected = Vec::new();
        let Some(receiver) = &self.receiver else {
            return Ok(collected);
        };
        match receiver.try_recv() {
            Ok(events) => {
                collected.extend(events);
                while let Ok(events) = receiver.try_recv() {
                    collected.extend(events);
                }
                Ok(collected)
            }
            Err(TryRecvError::Empty) => Ok(collected),
            Err(TryRecvError::Disconnected) => Err(WatcherStopped),
        }
    }

    /// Stop watching and join the background thread.
    pub fn stop(&mut self) {
        if let Some(sender) = self.stop_sender.take() {
            let _ = sender.send(());
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
        self.receiver = None;
    }
}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Error returned by [`FileWatcher::poll`] when the background watcher
/// thread has exited and the event channel is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatcherStopped;

impl std::fmt::Display for WatcherStopped {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "file watcher is no longer running")
    }
}

impl std::error::Error for WatcherStopped {}

/// The watcher thread's main loop: create the debounced watcher, start
/// watching `root`, signal readiness to [`FileWatcher::watch`], then block
/// until told to stop. Initialisation failures are signalled early so that
/// `watch()` can surface them instead of hanging. The debouncer invokes its
/// callback on notify's internal threads; this thread only exists to keep the
/// debouncer alive.
fn run_watcher_thread(
    root: &Path,
    event_sender: Sender<Vec<WatchEvent>>,
    stop_receiver: Receiver<()>,
    ready_sender: Sender<Result<(), String>>,
) {
    let watch_root = root.to_owned();
    // `None` tick rate lets the debouncer pick its own (timeout/5), which is
    // the recommended default.
    let mut debouncer = match new_debouncer(
        DEBOUNCE_INTERVAL,
        None,
        move |result: DebounceEventResult| {
            let mut events: Vec<WatchEvent> = Vec::new();
            match result {
                Ok(debounced_events) => {
                    for debounced_event in debounced_events {
                        collect_events(&watch_root, &debounced_event, &mut events);
                    }
                }
                Err(errors) => {
                    for error in errors {
                        log::error!("FileWatcher: debouncer error: {error}");
                    }
                }
            }
            if !events.is_empty() {
                let _ = event_sender.send(events);
            }
        },
    ) {
        Ok(debouncer) => debouncer,
        Err(error) => {
            let _ = ready_sender.send(Err(error.to_string()));
            return;
        }
    };

    if let Err(error) = debouncer.watch(root, RecursiveMode::Recursive) {
        let _ = ready_sender.send(Err(error.to_string()));
        return;
    }

    // Signal that the watcher is started before blocking, so `watch()` can
    // return to the caller.
    let _ = ready_sender.send(Ok(()));

    // Block until stop() is called, keeping the debouncer alive.
    let _ = stop_receiver.recv();
}

/// Translate a debounced event into [`WatchEvent`]s.
///
/// The debouncer emits `notify::Event`s (a kind plus a list of absolute
/// paths); we translate them into index-relevant [`WatchEvent`]s. Filtering
/// rules (mirroring the scanner's behaviour):
/// - hidden paths (any component starting with `.`) are always skipped;
/// - create/modify events only pass for Markdown files;
/// - remove events pass for any path, so a deleted directory can cascade;
/// - a rename's destination passes regardless of extension, so a renamed
///   directory can be rescanned.
///
/// Renames arrive as `ModifyKind::Name(...)` events: `From` carries the old
/// path, `To` the new one, and `Both` a pair `[old, new]` when the
/// debouncer could correlate the two sides. On macOS FSEvents the source
/// path cannot be correlated, so only `Any` with the new path is produced;
/// the stale index entry for the old path remains until the next full
/// re-scan.
///
/// Paths are converted to be relative to `root`. No filesystem calls are
/// made: whether a path is a file or a directory is decided by the consumer.
fn collect_events(root: &Path, debounced_event: &DebouncedEvent, events: &mut Vec<WatchEvent>) {
    let event = &debounced_event.event;
    match &event.kind {
        // Renames are reported as Modify(Name(...)); handle them separately
        // from ordinary content changes.
        EventKind::Modify(ModifyKind::Name(rename_mode)) => match rename_mode {
            // The old path disappears from the index.
            RenameMode::From => {
                for path in &event.paths {
                    if is_non_hidden(root, path) {
                        events.push(WatchEvent::Removed(relative_of(root, path)));
                    }
                }
            }
            // The destination appears; forward it unconditionally: it may
            // be a Markdown file or a directory whose subtree needs a
            // rescan.
            RenameMode::To | RenameMode::Any => {
                for path in &event.paths {
                    if is_non_hidden(root, path) {
                        events.push(WatchEvent::Changed(relative_of(root, path)));
                    }
                }
            }
            RenameMode::Both => {
                // `paths` holds [old, new] when the debouncer could
                // correlate the two sides of the rename. When both are
                // visible, emit a paired `Renamed`; otherwise treat each
                // path as a generic change.
                match &event.paths[..] {
                    [from, to] if is_non_hidden(root, from) && is_non_hidden(root, to) => {
                        events.push(WatchEvent::Renamed(
                            relative_of(root, from),
                            relative_of(root, to),
                        ));
                    }
                    _ => {
                        for path in &event.paths {
                            if is_non_hidden(root, path) {
                                events.push(WatchEvent::Changed(relative_of(root, path)));
                            }
                        }
                    }
                }
            }
            RenameMode::Other => {
                for path in &event.paths {
                    if is_non_hidden(root, path) {
                        events.push(WatchEvent::Changed(relative_of(root, path)));
                    }
                }
            }
        },
        // Content changes and creations both mean "re-index this file".
        EventKind::Create(_) | EventKind::Modify(_) => {
            for path in &event.paths {
                if is_non_hidden(root, path) && is_markdown(path) {
                    events.push(WatchEvent::Changed(relative_of(root, path)));
                }
            }
        }
        // Removals pass for any path, so a deleted directory can cascade.
        EventKind::Remove(_) => {
            for path in &event.paths {
                if is_non_hidden(root, path) {
                    events.push(WatchEvent::Removed(relative_of(root, path)));
                }
            }
        }
        // Reads and unclassified events carry no index-relevant information.
        EventKind::Access(_) | EventKind::Any | EventKind::Other => {}
    }
}

fn relative_of(root: &Path, path: &Path) -> PathBuf {
    // notify reports absolute paths; fall back to the raw path if it is not
    // under the root (should not happen for events from our own watcher).
    path.strip_prefix(root).unwrap_or(path).to_owned()
}

fn is_non_hidden(root: &Path, path: &Path) -> bool {
    !is_hidden(&relative_of(root, path))
}

pub(crate) fn is_markdown(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn is_hidden(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| name.starts_with('.'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, DataChange, RemoveKind};
    use std::time::Instant;

    fn debounced_event(kind: EventKind, paths: Vec<PathBuf>) -> DebouncedEvent {
        let event = paths
            .into_iter()
            .fold(notify::Event::new(kind), |event, path| event.add_path(path));
        DebouncedEvent {
            event,
            time: Instant::now(),
        }
    }

    /// Collect the [`WatchEvent`]s produced for a single synthetic debounced
    /// event rooted at `/workspace`. Paths are given relative to the root.
    fn collect(kind: EventKind, paths: &[&str]) -> Vec<WatchEvent> {
        let root = Path::new("/workspace");
        let absolute_paths: Vec<PathBuf> = paths
            .iter()
            .map(|path| Path::new("/workspace").join(path))
            .collect();
        let mut events = Vec::new();
        collect_events(root, &debounced_event(kind, absolute_paths), &mut events);
        events
    }

    #[test]
    fn create_and_modify_become_changed() {
        let expected = vec![WatchEvent::Changed(PathBuf::from("notes/rust.md"))];
        assert_eq!(
            collect(EventKind::Create(CreateKind::File), &["notes/rust.md"]),
            expected
        );
        assert_eq!(
            collect(
                EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                &["notes/rust.md"]
            ),
            expected
        );
    }

    #[test]
    fn remove_becomes_removed() {
        assert_eq!(
            collect(EventKind::Remove(RemoveKind::File), &["draft.md"]),
            vec![WatchEvent::Removed(PathBuf::from("draft.md"))]
        );
    }

    #[test]
    fn rename_from_emits_removed_for_the_old_path() {
        assert_eq!(
            collect(
                EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                &["old.md"]
            ),
            vec![WatchEvent::Removed(PathBuf::from("old.md"))]
        );
    }

    #[test]
    fn rename_to_emits_changed_for_the_new_path() {
        assert_eq!(
            collect(
                EventKind::Modify(ModifyKind::Name(RenameMode::To)),
                &["new.md"]
            ),
            vec![WatchEvent::Changed(PathBuf::from("new.md"))]
        );
    }

    #[test]
    fn rename_both_emits_a_paired_renamed_event() {
        // The debouncer correlates From + To into one Modify(Name(Both))
        // event whose paths hold [old, new].
        assert_eq!(
            collect(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                &["old.md", "new.md"]
            ),
            vec![WatchEvent::Renamed(
                PathBuf::from("old.md"),
                PathBuf::from("new.md")
            )]
        );
    }

    #[test]
    fn rename_destination_is_forwarded_even_when_not_markdown() {
        // The destination may be a directory; the indexer decides whether to
        // rescan a subtree or ignore the path.
        assert_eq!(
            collect(
                EventKind::Modify(ModifyKind::Name(RenameMode::To)),
                &["renamed-folder"]
            ),
            vec![WatchEvent::Changed(PathBuf::from("renamed-folder"))]
        );
    }

    #[test]
    fn non_markdown_files_are_filtered() {
        assert_eq!(
            collect(EventKind::Create(CreateKind::File), &["style.css"]),
            vec![]
        );
        assert_eq!(
            collect(
                EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                &["data.json"]
            ),
            vec![]
        );
    }

    #[test]
    fn hidden_files_and_directories_are_filtered() {
        assert_eq!(
            collect(EventKind::Create(CreateKind::File), &[".hidden.md"]),
            vec![]
        );
        assert_eq!(
            collect(
                EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                &["sub/.hidden.md"]
            ),
            vec![]
        );
        assert_eq!(
            collect(EventKind::Remove(RemoveKind::File), &[".hidden.md"]),
            vec![]
        );
    }

    #[test]
    fn access_other_and_any_are_ignored() {
        assert_eq!(
            collect(EventKind::Access(AccessKind::Read), &["read.md"]),
            vec![]
        );
        assert_eq!(collect(EventKind::Other, &[]), vec![]);
        assert_eq!(collect(EventKind::Any, &[]), vec![]);
    }

    #[test]
    fn multi_path_events_emit_changed_for_each_markdown_path() {
        let root = Path::new("/workspace");
        let event = notify::Event::new(EventKind::Create(CreateKind::File))
            .add_path(PathBuf::from("/workspace/a.md"))
            .add_path(PathBuf::from("/workspace/b.md"))
            .add_path(PathBuf::from("/workspace/ignored.txt"));
        let debounced = DebouncedEvent {
            event,
            time: Instant::now(),
        };
        let mut events = Vec::new();
        collect_events(root, &debounced, &mut events);
        assert_eq!(
            events,
            vec![
                WatchEvent::Changed(PathBuf::from("a.md")),
                WatchEvent::Changed(PathBuf::from("b.md")),
            ]
        );
    }

    #[test]
    fn absolute_paths_are_converted_to_relative() {
        assert_eq!(
            collect(EventKind::Create(CreateKind::File), &["notes/rust.md"]),
            vec![WatchEvent::Changed(PathBuf::from("notes/rust.md"))]
        );
    }

    /// Poll repeatedly until an event appears or `timeout` elapses.
    fn wait_for_events(watcher: &FileWatcher, timeout: Duration) -> Vec<WatchEvent> {
        let deadline = std::time::Instant::now() + timeout;
        let mut collected = Vec::new();
        while std::time::Instant::now() < deadline {
            if let Ok(events) = watcher.poll() {
                collected.extend(events);
                if !collected.is_empty() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        collected
    }

    /// Whether the given events report the deletion of `created.md`.
    ///
    /// macOS FSEvents reports unlinks as rename events, which the collector
    /// translates to `Changed`; the indexer resolves the ambiguity by
    /// checking whether the path still exists. Other platforms report the
    /// removal as `Removed` directly.
    fn removal_reported(events: &[WatchEvent]) -> bool {
        let created_md = PathBuf::from("created.md");
        if events.contains(&WatchEvent::Removed(created_md.clone())) {
            return true;
        }
        cfg!(target_os = "macos")
            && events.iter().any(|event| match event {
                WatchEvent::Changed(path) => path == &created_md,
                _ => false,
            })
    }

    #[test]
    fn watcher_reports_real_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut watcher = FileWatcher::new(dir.path());
        watcher.watch().unwrap();

        let created_path = dir.path().join("created.md");
        std::fs::write(&created_path, "# Created\n").unwrap();
        let events = wait_for_events(&watcher, Duration::from_secs(5));
        assert!(
            events.contains(&WatchEvent::Changed(PathBuf::from("created.md"))),
            "expected a change event for created.md, got {events:?}"
        );

        std::fs::write(&created_path, "# Created again\n").unwrap();
        let events = wait_for_events(&watcher, Duration::from_secs(5));
        assert!(
            events.contains(&WatchEvent::Changed(PathBuf::from("created.md"))),
            "expected a change event for the modification, got {events:?}"
        );

        std::fs::remove_file(&created_path).unwrap();
        let events = wait_for_events(&watcher, Duration::from_secs(5));
        assert!(
            removal_reported(&events),
            "expected a remove event, got {events:?}"
        );

        watcher.stop();
    }

    #[test]
    fn watcher_ignores_non_markdown_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut watcher = FileWatcher::new(dir.path());
        watcher.watch().unwrap();

        let other_path = dir.path().join("note.txt");
        std::fs::write(&other_path, "not markdown\n").unwrap();
        let events = wait_for_events(&watcher, Duration::from_secs(2));
        assert!(
            events.is_empty(),
            "expected no events for a non-Markdown file, got {events:?}"
        );

        watcher.stop();
    }
}
