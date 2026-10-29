//! Editing hosts: elements whose subtree an editor written in Rust edits.
//!
//! An editing host plays the role of a `contenteditable` element without the
//! browser's own editing actions. Blitz lays out and paints the host's
//! subtree as usual. The host gets the DOM events of its subtree as their
//! default action (after the event handlers ran and did not cancel them), and
//! changes the DOM itself. Blitz adds what a browser gives a
//! `contenteditable` element:
//!
//! - IME is enabled while the host or a node in it has focus,
//! - pasted text goes to the focused host,
//! - the host learns when the native text selection changed (the DOM's
//!   `selectionchange`),
//! - a collapsed selection inside the focused host is painted as a caret.

use blitz_traits::events::DomEvent;
use blitz_traits::node_id::NodeId;

use crate::{BaseDocument, LayoutOffset};

/// The behavior of an editing host.
///
/// Set it on an element with [`BaseDocument::set_editing_host`]. The element
/// needs to be focusable (for example with `tabindex`) to get key events.
pub trait EditingHost {
    /// The behavior was set on its element. Mount the initial content here.
    ///
    /// Dioxus sets the behavior while it creates the element, so the
    /// element may not be in the document yet.
    fn attached(&mut self, cx: EditingCx<'_>) {
        let _ = cx;
    }

    /// Handles an event targeted at the host or a node inside it.
    ///
    /// Runs as the event's default action. Returns `true` if the host handled
    /// the event, which skips Blitz's own default action (focus navigation
    /// with Tab, text selection with the pointer, and so on).
    fn handle_event(&mut self, event: &DomEvent, cx: EditingCx<'_>) -> bool {
        let _ = (event, cx);
        false
    }

    /// Inserts pasted text. Called while the host has focus.
    fn paste(&mut self, text: String, cx: EditingCx<'_>) {
        let _ = (text, cx);
    }

    /// The native text selection changed while the host had focus.
    ///
    /// Read it with [`BaseDocument::text_selection_offsets`].
    fn selection_changed(&mut self, cx: EditingCx<'_>) {
        let _ = cx;
    }
}

/// What an [`EditingHost`] callback gets: the document and the host element.
pub struct EditingCx<'a> {
    /// The document. The host's own entry is taken out while it runs.
    pub doc: &'a mut BaseDocument,
    /// The host element.
    pub host: NodeId,
}

impl BaseDocument {
    /// Makes `node_id` an editing host with the behavior `host`.
    ///
    /// Replaces an earlier host of the node.
    pub fn set_editing_host(&mut self, node_id: NodeId, host: Box<dyn EditingHost>) {
        self.editing_hosts.insert(node_id, Some(host));
        self.with_editing_host(node_id, |behavior, cx| behavior.attached(cx));
        if self
            .focus_node_id
            .is_some_and(|id| self.editing_host_of(id) == Some(node_id))
        {
            self.shell_provider.set_ime_enabled(true);
        }
    }

    /// Removes the editing host behavior of `node_id`, and returns it.
    pub fn remove_editing_host(&mut self, node_id: NodeId) -> Option<Box<dyn EditingHost>> {
        self.editing_hosts.remove(&node_id).flatten()
    }

    /// Returns the editing host that contains `node_id`, the node itself
    /// included.
    pub fn editing_host_of(&self, node_id: NodeId) -> Option<NodeId> {
        if self.editing_hosts.is_empty() {
            return None;
        }
        let mut id = Some(node_id);
        while let Some(current) = id {
            if self.editing_hosts.contains_key(&current) {
                return Some(current);
            }
            id = self.get_node(current)?.parent;
        }
        None
    }

    /// Returns the editing host that has focus or contains the focused node.
    pub fn focused_editing_host(&self) -> Option<NodeId> {
        self.editing_host_of(self.focus_node_id?)
    }

    /// Returns the anchor and focus of the native text selection as layout
    /// offsets, also for a collapsed selection.
    pub fn text_selection_offsets(&self) -> Option<(LayoutOffset, LayoutOffset)> {
        let lookup = |parent, index| self.find_anonymous_block_by_index(parent, index);
        let anchor = self.text_selection.anchor.resolve_node_id(lookup)?;
        let focus = self.text_selection.focus.resolve_node_id(lookup)?;
        Some((
            LayoutOffset {
                inline_root: anchor,
                offset: self.text_selection.anchor.offset,
            },
            LayoutOffset {
                inline_root: focus,
                offset: self.text_selection.focus.offset,
            },
        ))
    }

    /// Returns the caret rectangle to paint, in page coordinates (CSS px).
    ///
    /// There is a caret when the native text selection is collapsed and lies
    /// in the focused editing host.
    pub fn editing_caret_rect(&self) -> Option<kurbo::Rect> {
        let host = self.focused_editing_host()?;
        let (anchor, focus) = self.text_selection_offsets()?;
        if anchor != focus || self.editing_host_of(focus.inline_root) != Some(host) {
            return None;
        }
        self.caret_rect(focus)
    }

    /// Runs `f` with the host behavior of `host` taken out of the document.
    ///
    /// Returns `None` if `host` is no editing host, or if its behavior is
    /// already running (a callback that dispatches into its own host).
    pub(crate) fn with_editing_host<R>(
        &mut self,
        host: NodeId,
        f: impl FnOnce(&mut dyn EditingHost, EditingCx<'_>) -> R,
    ) -> Option<R> {
        let mut behavior = self.editing_hosts.get_mut(&host)?.take()?;
        let result = f(behavior.as_mut(), EditingCx { doc: self, host });
        // The callback may have removed or replaced the host.
        if let Some(slot @ None) = self.editing_hosts.get_mut(&host) {
            *slot = Some(behavior);
        }
        Some(result)
    }

    /// Gives a DOM event to the editing host that contains its target.
    ///
    /// Returns `true` if the host handled it. Afterwards the caller runs
    /// [`Self::notify_selection_change`].
    pub(crate) fn dispatch_to_editing_host(&mut self, event: &DomEvent) -> bool {
        let Some(host) = self.editing_host_of(event.target) else {
            return false;
        };
        self.with_editing_host(host, |behavior, cx| behavior.handle_event(event, cx))
            .unwrap_or(false)
    }

    /// Tells the focused editing host that the native text selection changed,
    /// if it differs from `before`, and moves the IME cursor area to the caret.
    pub(crate) fn notify_selection_change(&mut self, before: &crate::selection::TextSelection) {
        if self.text_selection == *before {
            return;
        }
        let Some(host) = self.focused_editing_host() else {
            return;
        };
        if let Some(rect) = self.editing_caret_rect() {
            self.shell_provider.set_ime_cursor_area(
                rect.x0 as f32,
                rect.y0 as f32,
                rect.width() as f32,
                rect.height() as f32,
            );
        }
        self.shell_provider.request_redraw();
        self.with_editing_host(host, |behavior, cx| behavior.selection_changed(cx));
    }
}
