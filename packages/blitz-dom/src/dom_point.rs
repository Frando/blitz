//! DOM boundary points and where they sit in the inline layout.
//!
//! A [`DomPoint`] is a DOM boundary point: a node and an offset, as in the
//! DOM Range and Selection APIs. Blitz lays text out per inline root, so a
//! position in the rendered text is a [`LayoutOffset`]: the inline root and a
//! byte offset into its layout text. White space collapsing and
//! `text-transform` make the layout text differ from the DOM text, so the
//! mapping walks the inline root's subtree.
//!
//! Editors (`contenteditable` hosts, rich text editors in Rust) need the
//! mapping in both directions: from a pointer position to a DOM point
//! (`caretPositionFromPoint`), and from a DOM point to a caret rectangle
//! (`Range.getClientRects` of a collapsed range).

use blitz_traits::node_id::NodeId;
use icu_properties::{
    CodePointMapData,
    props::{GeneralCategory, GeneralCategoryGroup},
};
use parley::{Affinity, Cursor};
use style::properties::longhands::white_space_collapse::computed_value::T as WhiteSpaceCollapse;
use style::values::computed::{Display, TextTransform};

use crate::{BaseDocument, CopyStyleGetters, Node, NodeData};

/// A DOM boundary point.
///
/// In a text node, `offset` counts UTF-16 code units of the node's text, as
/// in the DOM. In any other node it is a child index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DomPoint {
    /// The node that contains the point.
    pub node: NodeId,
    /// The offset in `node`.
    pub offset: usize,
}

impl DomPoint {
    /// Creates a point at `offset` in `node`.
    pub fn new(node: NodeId, offset: usize) -> Self {
        Self { node, offset }
    }
}

/// A position in the rendered text of an inline root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutOffset {
    /// The inline root whose layout holds the text.
    pub inline_root: NodeId,
    /// A byte offset into the inline root's layout text.
    pub offset: usize,
}

impl BaseDocument {
    /// Returns the layout offset of a DOM point.
    ///
    /// A point in a block container without inline layout of its own maps to
    /// the nearest text after it, else the nearest text before it. Returns
    /// `None` for a point outside the document or with no text near it.
    pub fn layout_offset_of(&self, point: DomPoint) -> Option<LayoutOffset> {
        let node = self.get_node(point.node)?;
        if !node.flags.is_in_document() {
            return None;
        }
        if let Some(root) = self.inline_root_of(node.id) {
            let mut result = None;
            self.walk_inline_root(root, |boundary, cursor| {
                let hit = match boundary {
                    Boundary::Text { node, units } => node == point.node && units >= point.offset,
                    Boundary::Child { node, index, last } => {
                        node == point.node && (last || index >= point.offset)
                    }
                };
                if hit {
                    result = Some(cursor);
                }
                hit
            });
            if let Some(offset) = result {
                return Some(LayoutOffset {
                    inline_root: root,
                    offset,
                });
            }
        }

        // Block containers have no inline layout of their own. Use the adjacent text.
        let offset = point.offset.min(node.children.len());
        node.children[offset..]
            .iter()
            .find_map(|&id| self.subtree_layout_offset(id, false))
            .or_else(|| {
                node.children[..offset]
                    .iter()
                    .rev()
                    .find_map(|&id| self.subtree_layout_offset(id, true))
            })
    }

    /// Returns the DOM point at a layout offset.
    ///
    /// Prefers a point in a text node: the end of the text before the offset
    /// wins over the start of the text after it, as a caret keeps the marks
    /// of the text before it. An offset with no text node at it (an empty
    /// line, a line break) maps to a child index in an element. Returns
    /// `None` if `at.inline_root` has no inline layout.
    pub fn dom_point_at(&self, at: LayoutOffset) -> Option<DomPoint> {
        let root = self.get_node(at.inline_root)?;
        root.element_data()?.inline_layout_data.as_ref()?;
        let mut text = None;
        let mut child = None;
        self.walk_inline_root(at.inline_root, |boundary, cursor| {
            if cursor < at.offset {
                return false;
            }
            match boundary {
                Boundary::Text { node, units } => {
                    text = Some(DomPoint::new(node, units));
                    true
                }
                Boundary::Child { node, index, .. } => {
                    child.get_or_insert(DomPoint::new(node, index));
                    false
                }
            }
        });
        text.or(child)
            .or(Some(DomPoint::new(at.inline_root, root.children.len())))
    }

    /// Returns the DOM point under a point in page coordinates (CSS px).
    ///
    /// This is the DOM's `caretPositionFromPoint`. Returns `None` if no text
    /// is under the point.
    pub fn dom_point_from_point(&self, x: f32, y: f32) -> Option<DomPoint> {
        let (inline_root, offset) = self.find_text_position(x, y)?;
        self.dom_point_at(LayoutOffset {
            inline_root,
            offset,
        })
    }

    /// Returns the caret rectangle at a layout offset, in page coordinates
    /// (CSS px).
    ///
    /// The rectangle has zero width and spans the line height. At a soft line
    /// break the caret sits at the start of the next line, as the caret of the
    /// character after the offset. Returns `None` if `at.inline_root` has no
    /// inline layout.
    pub fn caret_rect(&self, at: LayoutOffset) -> Option<kurbo::Rect> {
        let root = self.get_node(at.inline_root)?;
        let inline = root.element_data()?.inline_layout_data.as_ref()?;
        let layout = &inline.layout;
        let offset = at.offset.min(inline.text.len());
        let scale = f64::from(layout.scale());
        let bbox =
            Cursor::from_byte_index(layout, offset, Affinity::Downstream).geometry(layout, 0.0);

        let box_layout = root.final_layout();
        let origin = root.absolute_position(
            box_layout.padding.left + box_layout.border.left,
            box_layout.padding.top + box_layout.border.top + inline.block_offset,
        );
        Some(
            kurbo::Rect::new(
                bbox.x0 / scale,
                bbox.y0 / scale,
                bbox.x1 / scale,
                bbox.y1 / scale,
            ) + kurbo::Vec2::new(f64::from(origin.x), f64::from(origin.y)),
        )
    }

    /// Returns the nearest node at or above `id` that has an inline layout.
    fn inline_root_of(&self, mut id: NodeId) -> Option<NodeId> {
        loop {
            let node = self.get_node(id)?;
            if node
                .element_data()
                .is_some_and(|element| element.inline_layout_data.is_some())
            {
                return Some(id);
            }
            id = node.layout_parent.get()?;
        }
    }

    /// Returns the layout offset of the first (or with `end`, the last) text
    /// in the subtree of `id`.
    fn subtree_layout_offset(&self, id: NodeId, end: bool) -> Option<LayoutOffset> {
        let node = self.get_node(id)?;
        if let NodeData::Text(data) = &node.data {
            let offset = if end {
                data.content.encode_utf16().count()
            } else {
                0
            };
            return self.layout_offset_of(DomPoint::new(id, offset));
        }
        if end {
            node.children
                .iter()
                .rev()
                .find_map(|&id| self.subtree_layout_offset(id, end))
        } else {
            node.children
                .iter()
                .find_map(|&id| self.subtree_layout_offset(id, end))
        }
    }

    /// Walks the DOM boundaries of an inline root in tree order.
    ///
    /// Calls `visit` with each boundary and the layout offset at it, until
    /// `visit` returns `true`. Boundaries inside `::before` and `::after`
    /// content are not DOM points and are skipped, but their text advances
    /// the offset.
    fn walk_inline_root(&self, root: NodeId, visit: impl FnMut(Boundary, usize) -> bool) {
        let Some(text) = self
            .get_node(root)
            .and_then(|node| node.element_data())
            .and_then(|element| element.inline_layout_data.as_ref())
            .map(|inline| inline.text.as_str())
        else {
            return;
        };
        let mut walker = InlineWalker {
            doc: self,
            root,
            text,
            cursor: 0,
            collapsed_space: false,
            in_pseudo: false,
            visit,
            done: false,
        };
        walker.walk(root);
    }
}

/// A DOM boundary seen by [`InlineWalker`].
#[derive(Debug, Clone, Copy)]
enum Boundary {
    /// Before the UTF-16 unit `units` of a text node, or at its end.
    Text { node: NodeId, units: usize },
    /// Before child `index` of a node other than text, or after its last
    /// child (`last`).
    ///
    /// The last boundary also stands for any larger offset, which a node
    /// without children (a comment) can have.
    Child {
        node: NodeId,
        index: usize,
        last: bool,
    },
}

/// Walks an inline root's subtree and tracks the layout offset of each DOM
/// boundary.
///
/// It follows how inline layout builds the layout text: collapsed white
/// space, `text-transform`, `<br>` as a line feed, nested inline roots
/// (inline blocks) as no text.
struct InlineWalker<'a, F> {
    doc: &'a BaseDocument,
    root: NodeId,
    text: &'a str,
    /// Byte offset into `text` of the next character.
    cursor: usize,
    collapsed_space: bool,
    in_pseudo: bool,
    visit: F,
    done: bool,
}

impl<F: FnMut(Boundary, usize) -> bool> InlineWalker<'_, F> {
    fn emit(&mut self, boundary: Boundary) {
        if !self.done && !self.in_pseudo {
            self.done = (self.visit)(boundary, self.cursor);
        }
    }

    fn walk(&mut self, id: NodeId) {
        let Some(node) = self.doc.get_node(id) else {
            return;
        };
        if self.done
            || node
                .primary_styles()
                .is_some_and(|style| style.clone_display() == Display::None)
        {
            return;
        }
        if id != self.root
            && node
                .element_data()
                .is_some_and(|element| element.inline_layout_data.is_some())
        {
            return;
        }
        if let NodeData::Text(data) = &node.data {
            self.walk_text(node, &data.content);
            return;
        }
        if node
            .element_data()
            .is_some_and(|element| element.name.local == crate::local_name!("br"))
        {
            self.advance('\n', TextTransform::NONE, WhiteSpaceCollapse::Preserve);
            return;
        }
        self.walk_pseudo(node.before());
        for (index, &child) in node.children.iter().enumerate() {
            self.emit(Boundary::Child {
                node: id,
                index,
                last: false,
            });
            self.walk(child);
        }
        self.emit(Boundary::Child {
            node: id,
            index: node.children.len(),
            last: true,
        });
        self.walk_pseudo(node.after());
    }

    fn walk_pseudo(&mut self, pseudo: Option<NodeId>) {
        if let Some(pseudo) = pseudo {
            let outer = std::mem::replace(&mut self.in_pseudo, true);
            self.walk(pseudo);
            self.in_pseudo = outer;
        }
    }

    fn walk_text(&mut self, node: &Node, content: &str) {
        let style = node
            .parent
            .and_then(|id| self.doc.get_node(id))
            .and_then(Node::primary_styles);
        let transform = style
            .as_ref()
            .map_or(TextTransform::NONE, |style| style.clone_text_transform());
        let whitespace = style
            .as_ref()
            .map_or(WhiteSpaceCollapse::Collapse, |style| {
                style.clone_white_space_collapse()
            });
        let mut units = 0;
        for c in content.chars() {
            self.emit(Boundary::Text {
                node: node.id,
                units,
            });
            if self.done {
                return;
            }
            self.advance(c, transform, whitespace);
            units += c.len_utf16();
        }
        self.emit(Boundary::Text {
            node: node.id,
            units,
        });
    }

    fn advance(&mut self, c: char, transform: TextTransform, whitespace: WhiteSpaceCollapse) {
        let remaining = &self.text[self.cursor..];
        let collapsible = match whitespace {
            WhiteSpaceCollapse::Collapse => c.is_ascii_whitespace(),
            WhiteSpaceCollapse::PreserveBreaks => matches!(c, ' ' | '\t'),
            _ => false,
        };
        if collapsible {
            if !self.collapsed_space {
                if remaining.starts_with(' ') {
                    self.cursor += 1;
                } else if transform.contains(TextTransform::FULL_WIDTH)
                    && remaining.starts_with('\u{3000}')
                {
                    self.cursor += '\u{3000}'.len_utf8();
                }
            }
            self.collapsed_space = true;
            return;
        }
        self.collapsed_space = false;
        let mut bytes = [0; 4];
        let original = c.encode_utf8(&mut bytes);
        if remaining.starts_with(&*original) {
            self.cursor += original.len();
            return;
        }
        // Consume case expansions only when they occur in the actual layout text.
        let map_width = |c| {
            let c = if transform.contains(TextTransform::FULL_WIDTH) {
                crate::full_width(c)
            } else {
                c
            };
            if transform.contains(TextTransform::FULL_SIZE_KANA) {
                crate::full_size_kana(c)
            } else {
                c
            }
        };
        let mapped = if transform.contains(TextTransform::UPPERCASE)
            || transform.contains(TextTransform::CAPITALIZE)
        {
            matching_prefix(remaining, c.to_uppercase().map(map_width))
        } else if transform.contains(TextTransform::LOWERCASE) {
            matching_prefix(remaining, c.to_lowercase().map(map_width))
        } else {
            None
        };
        if let Some(bytes) = mapped {
            self.cursor += bytes;
        } else if !transform.is_empty()
            && GeneralCategoryGroup::Mark
                .contains(CodePointMapData::<GeneralCategory>::new().get(c))
        {
            // Tailored casing can remove combining marks (e.g. Greek uppercase acute).
        } else if let Some(rendered) = remaining.chars().next() {
            // math-auto, full-width, full-size-kana and one-to-one tailored case mappings preserve
            // character count.
            self.cursor += rendered.len_utf8();
        }
    }
}

/// Returns the byte length of the prefix of `text` that equals `mapped`.
fn matching_prefix(text: &str, mapped: impl Iterator<Item = char>) -> Option<usize> {
    let mut text = text.chars();
    let mut bytes = 0;
    for c in mapped {
        if text.next() != Some(c) {
            return None;
        }
        bytes += c.len_utf8();
    }
    Some(bytes)
}
