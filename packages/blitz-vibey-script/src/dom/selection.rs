//! Selection API bindings.

use std::cell::Cell;

use blitz_dom::{BaseDocument, DomPoint, Node, NodeId, node::NodeData};
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsValue, Trace, object::JsObject,
};

use super::{define_method, dom_ctx, dom_exception, js_str, node_id_of_value};

type Point = (NodeId, usize);
type Range = (NodeId, Option<usize>, usize, usize);

#[derive(Trace, Finalize, JsData)]
struct Selection {
    #[unsafe_ignore_trace]
    points: Cell<Option<[Point; 2]>>,
    #[unsafe_ignore_trace]
    rendered_range: Cell<Option<[Range; 2]>>,
}

pub(crate) fn get_selection(
    _: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    if let Some(selection) = &ctx.state.borrow().selection {
        return Ok(selection.clone().into());
    }
    let selection = JsObject::from_proto_and_data(
        Some(context.intrinsics().constructors().object().prototype()),
        Selection {
            points: Cell::new(None),
            rendered_range: Cell::new(None),
        },
    );
    define_method(
        &selection,
        "setBaseAndExtent",
        4,
        set_base_and_extent,
        context,
    );
    define_method(&selection, "toString", 0, to_string, context);
    define_method(&selection, "removeAllRanges", 0, remove_all_ranges, context);
    define_method(&selection, "empty", 0, remove_all_ranges, context);
    ctx.state.borrow_mut().selection = Some(selection.clone());
    Ok(selection.into())
}

fn selection_object(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .filter(|object| object.is::<Selection>())
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("`this` is not a Selection")
                .into()
        })
}

fn set_base_and_extent(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let selection = selection_object(this)?;
    if args.len() < 4 {
        return Err(JsNativeError::typ()
            .with_message("setBaseAndExtent requires four arguments")
            .into());
    }
    let anchor = node_id_of_value(&args[0])
        .ok_or_else(|| JsNativeError::typ().with_message("anchorNode is not a Node"))?;
    let focus = node_id_of_value(&args[2])
        .ok_or_else(|| JsNativeError::typ().with_message("focusNode is not a Node"))?;
    let points = [
        (anchor, args[1].to_u32(context)? as usize),
        (focus, args[3].to_u32(context)? as usize),
    ];
    let ctx = dom_ctx(context)?;
    let mut doc = ctx.doc.borrow_mut();
    for &(id, offset) in &points {
        let Some(node) = doc.get_node(id) else {
            return Ok(JsValue::undefined());
        };
        let length = match &node.data {
            NodeData::Text(text) => text.content.encode_utf16().count(),
            NodeData::Comment { contents } => contents.encode_utf16().count(),
            _ => node.children.len(),
        };
        if offset > length {
            return Err(dom_exception(
                context,
                "IndexSizeError",
                "Selection offset exceeds the node's length",
            ));
        }
    }
    if points.iter().any(|&(id, _)| {
        !doc.get_node(id)
            .is_some_and(|node| node.flags.is_in_document())
    }) {
        return Ok(JsValue::undefined());
    }
    doc.resolve(0.0);
    apply_points(&mut doc, points);
    let data = selection.downcast_ref::<Selection>().unwrap();
    data.points.set(Some(points));
    data.rendered_range.set(range_endpoints(&doc));
    Ok(JsValue::undefined())
}

fn remove_all_ranges(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = selection_object(this)?;
    let data = object.downcast_ref::<Selection>().unwrap();
    data.points.set(None);
    data.rendered_range.set(None);
    dom_ctx(context)?.doc.borrow_mut().clear_text_selection();
    Ok(JsValue::undefined())
}

fn to_string(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = selection_object(this)?;
    let data = object.downcast_ref::<Selection>().unwrap();
    let ctx = dom_ctx(context)?;
    let mut doc = ctx.doc.borrow_mut();
    // A pointer selection replaces a selection previously made by script.
    if range_endpoints(&doc) != data.rendered_range.get() {
        data.points.set(None);
    }
    doc.resolve(0.0);
    if let Some(points) = data.points.get() {
        apply_points(&mut doc, points);
        data.rendered_range.set(range_endpoints(&doc));
    }
    Ok(js_str(&doc.get_selected_text().unwrap_or_default()))
}

fn range_endpoints(doc: &BaseDocument) -> Option<[Range; 2]> {
    let ranges = doc.get_text_selection_ranges();
    let stable_range = |&(id, start, end): &(NodeId, usize, usize)| {
        let node = doc.get_node(id)?;
        // Anonymous inline roots are rebuilt, so compare their stable location.
        let (id, index) = if node.is_anonymous() {
            let parent = doc.get_node(node.parent?)?;
            let children = parent.layout_children.borrow();
            let index = children
                .as_ref()?
                .iter()
                .take_while(|&&child| child != node.id)
                .filter(|&&child| doc.get_node(child).is_some_and(Node::is_anonymous))
                .count();
            (parent.id, Some(index))
        } else {
            (id, None)
        };
        Some((id, index, start, end))
    };
    Some([
        stable_range(ranges.first()?)?,
        stable_range(ranges.last()?)?,
    ])
}

fn apply_points(doc: &mut BaseDocument, points: [Point; 2]) {
    match (
        rendered_point(doc, points[0]),
        rendered_point(doc, points[1]),
    ) {
        (Some((anchor, start)), Some((focus, end))) => {
            doc.set_text_selection(anchor, start, focus, end)
        }
        _ => doc.clear_text_selection(),
    }
}

fn rendered_point(doc: &BaseDocument, point: Point) -> Option<Point> {
    let at = doc.layout_offset_of(DomPoint::new(point.0, point.1))?;
    Some((at.inline_root, at.offset))
}
