//! Nearest typed-node-view event ownership.
//!
//! Browser events bubble through the editor surface even when they started in
//! component-owned buttons, inputs, or drag chrome. The only stable boundary
//! is the nearest manager-stamped node-view host in `Event::composed_path()`.
//! An outer editable node's outlet must never make a nested atom look like
//! editor-owned content.

use wasm_bindgen::JsCast;
use web_sys::{Element, Event};

/// Typed view shape relevant to event routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoundaryHostKind {
    Atom,
    Editable,
}

/// Metadata resolved by [`super::node_view_manager::NodeViewManager`] for one
/// stamped host in the composed path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BoundaryHost {
    pub instance_id: u64,
    pub position: Option<usize>,
    pub kind: BoundaryHostKind,
}

/// Which owner gets the browser event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeViewEventBoundary {
    /// No stamped typed-view host was encountered.
    Editor,
    /// The target is inside this nearest host's exact owned-content outlet.
    EditableOutlet {
        instance_id: u64,
        position: Option<usize>,
    },
    /// The target belongs to the component shell rather than editor content.
    Chrome {
        instance_id: u64,
        position: Option<usize>,
        kind: BoundaryHostKind,
        interactive: bool,
    },
}

impl NodeViewEventBoundary {
    /// Key/input/clipboard/drop handling remains editor-owned only outside
    /// typed views or inside the nearest editable outlet.
    pub(crate) fn editor_handles(self, default_prevented: bool) -> bool {
        if default_prevented {
            return false;
        }
        match self {
            Self::Editor => true,
            Self::EditableOutlet {
                instance_id,
                position,
            } => {
                let _ = (instance_id, position);
                true
            }
            Self::Chrome { .. } => false,
        }
    }

    /// An unconsumed pointer on non-interactive chrome selects the semantic
    /// node. Controls retain their browser/component default instead.
    pub(crate) fn pointer_selection(self, default_prevented: bool) -> Option<usize> {
        if default_prevented {
            return None;
        }
        match self {
            Self::Chrome {
                instance_id,
                position: Some(position),
                kind: BoundaryHostKind::Atom,
                interactive: false,
                ..
            }
            | Self::Chrome {
                instance_id,
                position: Some(position),
                kind: BoundaryHostKind::Editable,
                interactive: false,
                ..
            } => {
                let _ = instance_id;
                Some(position)
            }
            _ => None,
        }
    }
}

/// Pure composed-path tokens. The browser adapter below only translates DOM
/// elements into this closed shape; nearest-boundary precedence lives here so
/// it can be exhaustively host-tested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoundaryPathEntry {
    Other,
    InteractiveControl,
    OwnedOutlet(u64),
    Host(BoundaryHost),
}

fn classify_path(path: &[BoundaryPathEntry]) -> NodeViewEventBoundary {
    let Some((host_index, host)) = path.iter().enumerate().find_map(|(index, entry)| {
        let BoundaryPathEntry::Host(host) = entry else {
            return None;
        };
        Some((index, *host))
    }) else {
        return NodeViewEventBoundary::Editor;
    };

    let before_host = &path[..host_index];
    if before_host.iter().any(
        |entry| matches!(entry, BoundaryPathEntry::OwnedOutlet(owner) if *owner == host.instance_id),
    ) {
        return NodeViewEventBoundary::EditableOutlet {
            instance_id: host.instance_id,
            position: host.position,
        };
    }

    NodeViewEventBoundary::Chrome {
        instance_id: host.instance_id,
        position: host.position,
        kind: host.kind,
        interactive: before_host
            .iter()
            .any(|entry| matches!(entry, BoundaryPathEntry::InteractiveControl)),
    }
}

/// Adapt a live `composed_path` to the pure nearest-boundary classifier.
///
/// `resolve_host` recognizes manager stamps. `outlet_for` returns only that
/// exact instance's stored outlet; no selector or descendant scan is used.
pub(crate) fn classify_event(
    event: &Event,
    mut resolve_host: impl FnMut(&Element) -> Option<BoundaryHost>,
    mut outlet_for: impl FnMut(u64) -> Option<Element>,
) -> NodeViewEventBoundary {
    let elements = event
        .composed_path()
        .iter()
        .filter_map(|value| value.dyn_into::<Element>().ok())
        .collect::<Vec<_>>();

    let Some((host_index, host)) = elements
        .iter()
        .enumerate()
        .find_map(|(index, element)| resolve_host(element).map(|host| (index, host)))
    else {
        return NodeViewEventBoundary::Editor;
    };

    let outlet = outlet_for(host.instance_id);
    let mut entries = Vec::with_capacity(host_index + 1);
    for element in &elements[..host_index] {
        let entry = if outlet
            .as_ref()
            .is_some_and(|outlet| outlet.is_same_node(Some(element)))
        {
            BoundaryPathEntry::OwnedOutlet(host.instance_id)
        } else if is_interactive_control(element) {
            BoundaryPathEntry::InteractiveControl
        } else {
            BoundaryPathEntry::Other
        };
        entries.push(entry);
    }
    entries.push(BoundaryPathEntry::Host(host));
    classify_path(&entries)
}

fn is_interactive_control(element: &Element) -> bool {
    if matches!(
        element.local_name().as_str(),
        "button"
            | "input"
            | "select"
            | "textarea"
            | "option"
            | "a"
            | "summary"
            | "label"
            | "audio"
            | "video"
    ) {
        return true;
    }
    if element
        .get_attribute("contenteditable")
        .is_some_and(|value| !value.eq_ignore_ascii_case("false"))
        || element
            .get_attribute("draggable")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    matches!(
        element.get_attribute("role").as_deref(),
        Some(
            "button"
                | "checkbox"
                | "combobox"
                | "link"
                | "listbox"
                | "menuitem"
                | "option"
                | "radio"
                | "searchbox"
                | "slider"
                | "spinbutton"
                | "switch"
                | "tab"
                | "textbox"
                | "treeitem"
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTER: BoundaryHost = BoundaryHost {
        instance_id: 1,
        position: Some(4),
        kind: BoundaryHostKind::Editable,
    };
    const INNER: BoundaryHost = BoundaryHost {
        instance_id: 2,
        position: Some(9),
        kind: BoundaryHostKind::Atom,
    };

    #[test]
    fn no_typed_host_belongs_to_editor() {
        assert_eq!(
            classify_path(&[BoundaryPathEntry::Other]),
            NodeViewEventBoundary::Editor
        );
    }

    #[test]
    fn exact_nearest_owned_outlet_belongs_to_editor() {
        assert_eq!(
            classify_path(&[
                BoundaryPathEntry::Other,
                BoundaryPathEntry::OwnedOutlet(1),
                BoundaryPathEntry::Other,
                BoundaryPathEntry::Host(OUTER),
            ]),
            NodeViewEventBoundary::EditableOutlet {
                instance_id: 1,
                position: Some(4),
            }
        );
    }

    #[test]
    fn nested_atom_wins_before_outer_outlet() {
        assert_eq!(
            classify_path(&[
                BoundaryPathEntry::Other,
                BoundaryPathEntry::Host(INNER),
                BoundaryPathEntry::OwnedOutlet(1),
                BoundaryPathEntry::Host(OUTER),
            ]),
            NodeViewEventBoundary::Chrome {
                instance_id: 2,
                position: Some(9),
                kind: BoundaryHostKind::Atom,
                interactive: false,
            }
        );
    }

    #[test]
    fn outlet_from_another_instance_does_not_punch_through_boundary() {
        assert!(matches!(
            classify_path(&[
                BoundaryPathEntry::OwnedOutlet(1),
                BoundaryPathEntry::Host(INNER),
            ]),
            NodeViewEventBoundary::Chrome { instance_id: 2, .. }
        ));
    }

    #[test]
    fn chrome_controls_keep_browser_ownership() {
        let boundary = classify_path(&[
            BoundaryPathEntry::InteractiveControl,
            BoundaryPathEntry::Other,
            BoundaryPathEntry::Host(INNER),
        ]);
        assert!(!boundary.editor_handles(false));
        assert_eq!(boundary.pointer_selection(false), None);
    }

    #[test]
    fn unconsumed_shell_pointer_selects_but_prevented_pointer_does_not() {
        let boundary = classify_path(&[BoundaryPathEntry::Other, BoundaryPathEntry::Host(OUTER)]);
        assert_eq!(boundary.pointer_selection(false), Some(4));
        assert_eq!(boundary.pointer_selection(true), None);
    }

    #[test]
    fn default_prevented_disables_editor_action_inside_owned_content() {
        let boundary = classify_path(&[
            BoundaryPathEntry::OwnedOutlet(1),
            BoundaryPathEntry::Host(OUTER),
        ]);
        assert!(boundary.editor_handles(false));
        assert!(!boundary.editor_handles(true));
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod browser_tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use wasm_bindgen::closure::Closure;
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
    use web_sys::{CustomEvent, CustomEventInit};

    use super::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    fn composed_path_prefers_inner_atom_over_outer_editable_outlet() {
        let document = web_sys::window().unwrap().document().unwrap();
        let surface = document.create_element("div").unwrap();
        let outer_host = document.create_element("div").unwrap();
        let outer_shell = document.create_element("section").unwrap();
        let outer_outlet = document.create_element("div").unwrap();
        let inner_host = document.create_element("span").unwrap();
        let inner_shell = document.create_element("span").unwrap();
        let inner_label = document.create_element("span").unwrap();
        let outer_text = document.create_element("span").unwrap();

        inner_shell.append_child(&inner_label).unwrap();
        inner_host.append_child(&inner_shell).unwrap();
        outer_outlet.append_child(&inner_host).unwrap();
        outer_outlet.append_child(&outer_text).unwrap();
        outer_shell.append_child(&outer_outlet).unwrap();
        outer_host.append_child(&outer_shell).unwrap();
        surface.append_child(&outer_host).unwrap();
        document.body().unwrap().append_child(&surface).unwrap();

        let results = Rc::new(RefCell::new(Vec::new()));
        let results_for_listener = results.clone();
        let outer_for_listener = outer_host.clone();
        let inner_for_listener = inner_host.clone();
        let outlet_for_listener = outer_outlet.clone();
        let listener = Closure::wrap(Box::new(move |event: Event| {
            let boundary = classify_event(
                &event,
                |element| {
                    if element.is_same_node(Some(&inner_for_listener)) {
                        Some(BoundaryHost {
                            instance_id: 2,
                            position: Some(9),
                            kind: BoundaryHostKind::Atom,
                        })
                    } else if element.is_same_node(Some(&outer_for_listener)) {
                        Some(BoundaryHost {
                            instance_id: 1,
                            position: Some(4),
                            kind: BoundaryHostKind::Editable,
                        })
                    } else {
                        None
                    }
                },
                |instance_id| (instance_id == 1).then(|| outlet_for_listener.clone()),
            );
            results_for_listener.borrow_mut().push(boundary);
        }) as Box<dyn FnMut(Event)>);
        surface
            .add_event_listener_with_callback("boundary-probe", listener.as_ref().unchecked_ref())
            .unwrap();

        inner_label.dispatch_event(&bubbling_probe()).unwrap();
        outer_text.dispatch_event(&bubbling_probe()).unwrap();

        assert_eq!(
            results.borrow().as_slice(),
            [
                NodeViewEventBoundary::Chrome {
                    instance_id: 2,
                    position: Some(9),
                    kind: BoundaryHostKind::Atom,
                    interactive: false,
                },
                NodeViewEventBoundary::EditableOutlet {
                    instance_id: 1,
                    position: Some(4),
                },
            ]
        );

        surface.remove();
    }

    #[wasm_bindgen_test]
    fn composed_path_marks_nested_controls_as_component_owned() {
        let document = web_sys::window().unwrap().document().unwrap();
        let surface = document.create_element("div").unwrap();
        let host = document.create_element("span").unwrap();
        let button = document.create_element("button").unwrap();
        host.append_child(&button).unwrap();
        surface.append_child(&host).unwrap();
        document.body().unwrap().append_child(&surface).unwrap();

        let result = Rc::new(RefCell::new(None));
        let result_for_listener = result.clone();
        let host_for_listener = host.clone();
        let listener = Closure::wrap(Box::new(move |event: Event| {
            *result_for_listener.borrow_mut() = Some(classify_event(
                &event,
                |element| {
                    element
                        .is_same_node(Some(&host_for_listener))
                        .then_some(BoundaryHost {
                            instance_id: 7,
                            position: Some(12),
                            kind: BoundaryHostKind::Atom,
                        })
                },
                |_| None,
            ));
        }) as Box<dyn FnMut(Event)>);
        surface
            .add_event_listener_with_callback("boundary-probe", listener.as_ref().unchecked_ref())
            .unwrap();

        button.dispatch_event(&bubbling_probe()).unwrap();
        assert_eq!(
            *result.borrow(),
            Some(NodeViewEventBoundary::Chrome {
                instance_id: 7,
                position: Some(12),
                kind: BoundaryHostKind::Atom,
                interactive: true,
            })
        );

        surface.remove();
    }

    fn bubbling_probe() -> CustomEvent {
        let init = CustomEventInit::new();
        init.set_bubbles(true);
        init.set_composed(true);
        init.set_cancelable(true);
        CustomEvent::new_with_event_init_dict("boundary-probe", &init).unwrap()
    }
}
