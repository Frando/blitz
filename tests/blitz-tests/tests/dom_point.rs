//! Mapping between DOM boundary points, layout offsets and page coordinates
//! (`BaseDocument::layout_offset_of`, `dom_point_at`, `dom_point_from_point`,
//! `caret_rect`).

use blitz_dom::{BaseDocument, DocumentConfig, DomPoint, LayoutOffset, NodeId};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn make_doc(body: &str) -> HtmlDocument {
    let html = format!(
        r#"<!DOCTYPE html><html><head><style>
            body {{ margin: 0; font-size: 16px; line-height: 20px; }}
            p {{ margin: 0; }}
        </style></head><body>{body}</body></html>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn by_id(doc: &BaseDocument, id: &str) -> NodeId {
    doc.query_selector(&format!("#{id}"))
        .unwrap()
        .unwrap_or_else(|| panic!("#{id} not found"))
}

/// The first child of the element with `id`, which the tests make a text node.
fn text_in(doc: &BaseDocument, id: &str) -> NodeId {
    doc.get_node(by_id(doc, id)).unwrap().children[0]
}

fn layout_text(doc: &BaseDocument, root: NodeId) -> String {
    let node = doc.get_node(root).unwrap();
    let inline = node.element_data().unwrap().inline_layout_data.as_ref();
    inline.unwrap().text.clone()
}

const MARKS: &str = r#"<p id="p" style="padding: 10px 0 0 30px; border-left: 5px solid">Hello <b id="b">bold</b> and   <i id="i">more</i></p>"#;

#[test]
fn text_points_map_to_layout_offsets_and_back() {
    let doc = make_doc(MARKS);
    let p = by_id(&doc, "p");
    let hello = text_in(&doc, "p");
    let bold = text_in(&doc, "b");
    let more = text_in(&doc, "i");
    assert_eq!(layout_text(&doc, p), "Hello bold and more");

    let at = |offset| LayoutOffset {
        inline_root: p,
        offset,
    };
    assert_eq!(doc.layout_offset_of(DomPoint::new(hello, 0)), Some(at(0)));
    assert_eq!(doc.layout_offset_of(DomPoint::new(bold, 2)), Some(at(8)));
    assert_eq!(doc.layout_offset_of(DomPoint::new(more, 4)), Some(at(19)));

    // Between two text nodes the caret takes the end of the text before it.
    assert_eq!(doc.dom_point_at(at(6)), Some(DomPoint::new(hello, 6)));
    assert_eq!(doc.dom_point_at(at(10)), Some(DomPoint::new(bold, 4)));
    assert_eq!(doc.dom_point_at(at(7)), Some(DomPoint::new(bold, 1)));
    assert_eq!(doc.dom_point_at(at(19)), Some(DomPoint::new(more, 4)));

    // Every layout offset round-trips through its DOM point.
    for offset in 0..=19 {
        let point = doc.dom_point_at(at(offset)).unwrap();
        assert_eq!(doc.layout_offset_of(point), Some(at(offset)), "{point:?}");
    }
}

#[test]
fn element_points_map_to_adjacent_text() {
    let doc = make_doc(r#"<div id="d"><p id="a">One</p><p id="e"><br></p></div>"#);
    let div = by_id(&doc, "d");
    let a = by_id(&doc, "a");
    let e = by_id(&doc, "e");

    // A block container has no inline layout: use the text after the point.
    let start = doc.layout_offset_of(DomPoint::new(div, 0)).unwrap();
    assert_eq!(start.inline_root, a);
    assert_eq!(start.offset, 0);

    // An empty line has no text node: its point is a child index.
    let empty = LayoutOffset {
        inline_root: e,
        offset: 0,
    };
    assert_eq!(doc.dom_point_at(empty), Some(DomPoint::new(e, 0)));
    assert_eq!(doc.layout_offset_of(DomPoint::new(e, 0)), Some(empty));
}

#[test]
fn caret_rects_and_hits_agree() {
    let doc = make_doc(MARKS);
    let p = by_id(&doc, "p");
    let mut last_x = f64::MIN;
    for offset in 0..19 {
        let at = LayoutOffset {
            inline_root: p,
            offset,
        };
        let rect = doc.caret_rect(at).unwrap();
        // Padding and border move the text: the first caret is at x = 35,
        // and its line box starts at y = 10 (the caret overhangs it a little).
        if offset == 0 {
            assert!((rect.x0 - 35.0).abs() < 0.01, "{rect:?}");
            assert!((rect.center().y - 20.0).abs() < 2.0, "{rect:?}");
        }
        assert!(
            rect.x0 > last_x,
            "caret {offset} at {rect:?} is not right of {last_x}"
        );
        last_x = rect.x0;
        assert!(rect.height() > 10.0, "{rect:?}");

        // Just right of the caret is the left half of the character after it.
        let point = doc
            .dom_point_from_point(rect.x0 as f32 + 1.0, rect.center().y as f32)
            .unwrap();
        assert_eq!(doc.dom_point_at(at), Some(point), "offset {offset}");
    }
}

#[test]
fn caret_at_a_soft_wrap_is_on_the_next_line() {
    let doc = make_doc(r#"<p id="p" style="width: 60px">aaaa bbbb</p>"#);
    let p = by_id(&doc, "p");
    let at = |offset| LayoutOffset {
        inline_root: p,
        offset,
    };
    let first = doc.caret_rect(at(0)).unwrap();
    let wrapped = doc.caret_rect(at(5)).unwrap();
    assert!(
        (wrapped.y0 - first.y0 - 20.0).abs() < 0.01,
        "{first:?} {wrapped:?}"
    );
    assert!(
        (wrapped.x0 - first.x0).abs() < 0.01,
        "{first:?} {wrapped:?}"
    );
}
