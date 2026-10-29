//! Editing hosts (`BaseDocument::set_editing_host`): an editor in Rust that
//! edits a subtree of the document, as a `contenteditable` element without
//! the browser's own editing.
//!
//! `TinyEditor` is a minimal host: it inserts typed and pasted text at the
//! caret and moves the caret with the arrow keys, all through DOM points,
//! layout offsets and the native text selection.

use blitz_dom::{
    BaseDocument, Document, DocumentConfig, DomPoint, EditingCx, EditingHost, LayoutOffset,
    NodeData, NodeId,
};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_test_harness::{key_event, mouse_pointer_event};
use blitz_traits::{
    events::{
        BlitzImeEvent, DomEvent, DomEventData, KeyState, MouseEventButton, MouseEventButtons,
        UiEvent,
    },
    shell::{ColorScheme, ShellProvider, Viewport},
};
use keyboard_types::{Key, Modifiers, NamedKey};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

/// Records the IME requests of the document.
#[derive(Default)]
struct ImeShell {
    enabled: Mutex<Option<bool>>,
    cursor_area: Mutex<Option<(f32, f32)>>,
}

impl ShellProvider for ImeShell {
    fn set_ime_enabled(&self, is_enabled: bool) {
        *self.enabled.lock().unwrap() = Some(is_enabled);
    }
    fn set_ime_cursor_area(&self, x: f32, y: f32, _width: f32, _height: f32) {
        *self.cursor_area.lock().unwrap() = Some((x, y));
    }
}

/// What the host saw, for the test to check.
#[derive(Default)]
struct Log {
    selection_changes: usize,
}

struct TinyEditor {
    log: Rc<RefCell<Log>>,
}

impl TinyEditor {
    /// Returns the caret as a DOM point in a text node.
    fn caret(doc: &BaseDocument) -> Option<(LayoutOffset, DomPoint)> {
        let (_, focus) = doc.text_selection_offsets()?;
        Some((focus, doc.dom_point_at(focus)?))
    }

    /// Puts a collapsed selection at a layout offset.
    fn set_caret(doc: &mut BaseDocument, at: LayoutOffset) {
        doc.set_text_selection(at.inline_root, at.offset, at.inline_root, at.offset);
    }

    fn insert(&mut self, doc: &mut BaseDocument, text: &str) {
        let Some((_, point)) = Self::caret(doc) else {
            return;
        };
        let NodeData::Text(data) = &doc.get_node(point.node).unwrap().data else {
            return;
        };
        let old = data.content.clone();
        let split = old
            .char_indices()
            .scan(0, |units, (i, c)| {
                let at = *units;
                *units += c.len_utf16();
                Some((i, at))
            })
            .find(|&(_, units)| units == point.offset)
            .map_or(old.len(), |(i, _)| i);
        let new = format!("{}{text}{}", &old[..split], &old[split..]);
        doc.mutate().set_node_text(point.node, &new);
        doc.resolve(0.0);
        let after = DomPoint::new(point.node, point.offset + text.encode_utf16().count());
        if let Some(at) = doc.layout_offset_of(after) {
            Self::set_caret(doc, at);
        }
    }

    fn move_caret(doc: &mut BaseDocument, forward: bool) {
        let Some((at, _)) = Self::caret(doc) else {
            return;
        };
        let offset = if forward {
            at.offset + 1
        } else {
            at.offset.saturating_sub(1)
        };
        Self::set_caret(
            doc,
            LayoutOffset {
                inline_root: at.inline_root,
                offset,
            },
        );
    }
}

impl EditingHost for TinyEditor {
    fn handle_event(&mut self, event: &DomEvent, cx: EditingCx<'_>) -> bool {
        let DomEventData::KeyDown(key) = &event.data else {
            // Pointer events keep Blitz's default: focus and text selection.
            return false;
        };
        match &key.key {
            Key::Named(NamedKey::ArrowLeft) => Self::move_caret(cx.doc, false),
            Key::Named(NamedKey::ArrowRight) => Self::move_caret(cx.doc, true),
            Key::Character(text) => self.insert(cx.doc, text),
            _ => {}
        }
        // The editor claims every key, Tab included.
        true
    }

    fn paste(&mut self, text: String, cx: EditingCx<'_>) {
        self.insert(cx.doc, &text);
    }

    fn selection_changed(&mut self, _cx: EditingCx<'_>) {
        self.log.borrow_mut().selection_changes += 1;
    }
}

struct Setup {
    doc: HtmlDocument,
    shell: Arc<ImeShell>,
    log: Rc<RefCell<Log>>,
    host: NodeId,
    text: NodeId,
}

fn setup() -> Setup {
    let shell = Arc::new(ImeShell::default());
    let mut doc = HtmlDocument::from_html(
        r#"<!DOCTYPE html><html><head><style>
            body { margin: 0; font-size: 16px; line-height: 20px; }
            p { margin: 0; }
        </style></head><body>
            <div id="host" tabindex="0"><p id="p">Hello</p></div>
            <button id="other">Other</button>
        </body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            shell_provider: Some(shell.clone() as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let host = doc.query_selector("#host").unwrap().unwrap();
    let p = doc.query_selector("#p").unwrap().unwrap();
    let text = doc.get_node(p).unwrap().children[0];
    let log = Rc::new(RefCell::new(Log::default()));
    doc.set_editing_host(host, Box::new(TinyEditor { log: log.clone() }));
    Setup {
        doc,
        shell,
        log,
        host,
        text,
    }
}

fn text_of(doc: &BaseDocument, id: NodeId) -> String {
    match &doc.get_node(id).unwrap().data {
        NodeData::Text(data) => data.content.to_string(),
        _ => panic!("not a text node"),
    }
}

/// Clicks at the caret position of a layout offset.
fn click_at_offset(doc: &mut HtmlDocument, at: LayoutOffset) {
    let rect = doc.caret_rect(at).unwrap();
    let (x, y) = (rect.x0 as f32 + 0.5, rect.center().y as f32);
    let mut down = mouse_pointer_event(x, y);
    down.buttons = MouseEventButtons::Primary;
    doc.handle_ui_event(UiEvent::PointerDown(down));
    let mut up = mouse_pointer_event(x, y);
    up.button = MouseEventButton::Main;
    up.buttons = MouseEventButtons::None;
    doc.handle_ui_event(UiEvent::PointerUp(up));
}

fn press(doc: &mut HtmlDocument, key: Key) {
    doc.handle_ui_event(UiEvent::KeyDown(key_event(
        key.clone(),
        KeyState::Pressed,
        Modifiers::empty(),
    )));
    doc.handle_ui_event(UiEvent::KeyUp(key_event(
        key,
        KeyState::Released,
        Modifiers::empty(),
    )));
}

#[test]
fn a_click_focuses_the_host_and_places_a_caret() {
    let Setup {
        mut doc,
        shell,
        log,
        host,
        text,
    } = setup();
    let p = doc.get_node(text).unwrap().parent.unwrap();
    assert_eq!(doc.editing_caret_rect(), None);

    click_at_offset(
        &mut doc,
        LayoutOffset {
            inline_root: p,
            offset: 3,
        },
    );
    assert_eq!(doc.focused_editing_host(), Some(host));
    assert_eq!(*shell.enabled.lock().unwrap(), Some(true));
    assert!(log.borrow().selection_changes >= 1);

    let caret = doc.editing_caret_rect().expect("a caret");
    let expected = doc
        .caret_rect(LayoutOffset {
            inline_root: p,
            offset: 3,
        })
        .unwrap();
    assert_eq!(caret, expected);
    let (x, y) = shell
        .cursor_area
        .lock()
        .unwrap()
        .expect("an IME cursor area");
    assert_eq!((x as f64, y as f64), (caret.x0, caret.y0));
}

#[test]
fn keys_ime_and_paste_edit_the_dom_at_the_caret() {
    let Setup {
        mut doc,
        shell,
        text,
        host,
        ..
    } = setup();
    let p = doc.get_node(text).unwrap().parent.unwrap();
    let at = |offset| LayoutOffset {
        inline_root: p,
        offset,
    };
    click_at_offset(&mut doc, at(3));

    press(&mut doc, Key::Character("X".into()));
    assert_eq!(text_of(&doc, text), "HelXlo");
    assert_eq!(doc.text_selection_offsets(), Some((at(4), at(4))));

    press(&mut doc, Key::Named(NamedKey::ArrowLeft));
    press(&mut doc, Key::Named(NamedKey::ArrowLeft));
    doc.handle_ui_event(UiEvent::Paste("YZ".into()));
    assert_eq!(text_of(&doc, text), "HeYZlXlo");
    assert_eq!(doc.text_selection_offsets(), Some((at(4), at(4))));

    // The host claims Tab, so the focus stays.
    press(&mut doc, Key::Named(NamedKey::Tab));
    assert_eq!(doc.focused_editing_host(), Some(host));

    // IME commits reach the host as DOM events too; this host ignores them.
    doc.handle_ui_event(UiEvent::Ime(BlitzImeEvent::Commit("Q".into())));
    assert_eq!(text_of(&doc, text), "HeYZlXlo");

    // Moving the focus out of the host disables IME.
    let other = doc.query_selector("#other").unwrap().unwrap();
    doc.set_focus_to(other);
    assert_eq!(doc.focused_editing_host(), None);
    assert_eq!(*shell.enabled.lock().unwrap(), Some(false));
    assert_eq!(doc.editing_caret_rect(), None);
}

/// A host that only takes the focus and the selection.
struct Plain;
impl EditingHost for Plain {}

fn dioxus_app() -> dioxus::prelude::Element {
    use dioxus::prelude::*;
    let host = use_hook(|| dioxus_native_dom::EditingHostAttr::new(Plain));
    rsx! {
        div { id: "host", tabindex: "0", "__editing_host": host,
            p { id: "p", style: "margin: 0;", "Hello" }
        }
    }
}

#[test]
fn a_dioxus_attribute_makes_an_editing_host() {
    let mut harness = blitz_test_harness::Harness::from_component(dioxus_app);
    harness.click("#p");
    let host = harness.node("#host");
    let doc = harness.base();
    assert_eq!(doc.focused_editing_host(), Some(host));
    assert!(doc.editing_caret_rect().is_some());
}

#[test]
fn the_caret_blinks_until_it_rests() {
    use blitz_dom::{CARET_BLINK_PHASE, CARET_BLINK_TIMEOUT, caret_visible_after};
    use std::time::Duration;

    let Setup { mut doc, text, .. } = setup();
    let p = doc.get_node(text).unwrap().parent.unwrap();
    assert!(!doc.is_animating());
    click_at_offset(
        &mut doc,
        LayoutOffset {
            inline_root: p,
            offset: 2,
        },
    );
    // A caret that just moved is shown and needs redraws to blink.
    assert!(doc.editing_caret_visible());
    assert!(doc.is_animating());

    let ms = Duration::from_millis;
    assert!(caret_visible_after(ms(0)));
    assert!(!caret_visible_after(CARET_BLINK_PHASE + ms(1)));
    assert!(caret_visible_after(CARET_BLINK_PHASE * 2 + ms(1)));
    assert!(caret_visible_after(CARET_BLINK_TIMEOUT + CARET_BLINK_PHASE));
}

#[test]
fn the_application_reaches_its_host_by_type() {
    let Setup {
        mut doc, host, log, ..
    } = setup();
    let changes = doc.with_editing_host_as::<TinyEditor, _>(host, |editor, _cx| {
        editor.log.borrow().selection_changes
    });
    assert_eq!(changes, Some(0));
    assert!(doc
        .with_editing_host_as::<Plain, _>(host, |_, _| ())
        .is_none());
    assert_eq!(log.borrow().selection_changes, 0);
}

#[test]
fn a_host_dropped_with_its_parent_is_gone() {
    let Setup { mut doc, host, .. } = setup();
    let body = doc.get_node(host).unwrap().parent.unwrap();
    let mut m = doc.mutate();
    let wrapper = m.create_element(
        blitz_dom::QualName::new(None, blitz_dom::ns!(html), blitz_dom::local_name!("div")),
        Vec::new(),
    );
    m.append_children(body, &[wrapper]);
    m.remove_node(host);
    m.append_children(wrapper, &[host]);
    m.remove_and_drop_node(wrapper);
    drop(m);
    assert_eq!(doc.editing_host_of(host), None);
}
