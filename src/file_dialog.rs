//! Non-blocking native file/folder dialogs driven from egui frames so Linux Wayland, macOS,
//! and Windows never stall the main window event loop while a chooser is open.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

struct RepaintWaker(egui::Context);

impl Wake for RepaintWaker {
    fn wake(self: Arc<Self>) {
        self.0.request_repaint();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.request_repaint();
    }
}

/// An in-flight native file or folder chooser.
pub struct PendingDialog {
    fut: Option<Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>>>,
    ready: Option<Option<PathBuf>>,
    waker: Waker,
}

impl PendingDialog {
    pub fn pick_folder(dialog: rfd::AsyncFileDialog, ctx: &egui::Context) -> Self {
        Self::start(Box::pin(dialog.pick_folder()), ctx)
    }

    pub fn pick_file(dialog: rfd::AsyncFileDialog, ctx: &egui::Context) -> Self {
        Self::start(Box::pin(dialog.pick_file()), ctx)
    }

    pub fn save_file(dialog: rfd::AsyncFileDialog, ctx: &egui::Context) -> Self {
        Self::start(Box::pin(dialog.save_file()), ctx)
    }

    fn start(
        fut: Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>>,
        ctx: &egui::Context,
    ) -> Self {
        let mut pending = Self {
            fut: Some(fut),
            ready: None,
            waker: Waker::from(Arc::new(RepaintWaker(ctx.clone()))),
        };
        pending.poll_inner();
        pending
    }

    fn poll_inner(&mut self) {
        if self.ready.is_some() {
            return;
        }
        let Some(fut) = self.fut.as_mut() else {
            return;
        };
        let mut cx = Context::from_waker(&self.waker);
        if let Poll::Ready(handle) = fut.as_mut().poll(&mut cx) {
            self.ready = Some(handle.map(|h| h.path().to_path_buf()));
            self.fut = None;
        }
    }

    /// Returns `Some(picked)` once completed (with `None` inside if cancelled), or `None` while
    /// the dialog is still open.
    pub fn poll(&mut self) -> Option<Option<PathBuf>> {
        self.poll_inner();
        self.ready.take()
    }
}
