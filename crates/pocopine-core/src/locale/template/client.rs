use std::{cell::RefCell, rc::Rc};

use js_sys::{Function, Reflect};
use pocopine_locale::{
    ArgumentKind, CatalogError, CompiledMessage, DateTimeArg, PluralArg, RenderedPart,
    TranslationError, Value,
};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Element, Node};

use super::TranslationPlan;
use crate::{
    ScopeId, effect,
    locale::client::{LocaleController, active},
    mount::track_effect_on,
};

fn controller(message: CompiledMessage) -> Result<LocaleController, TranslationError> {
    let ui = active()?;
    if ui.build_id() != message.build_id {
        return Err(CatalogError::BuildMismatch.into());
    }
    Ok(ui)
}

pub fn value(
    plan: &'static TranslationPlan,
    proxy: &JsValue,
    root: Option<&crate::expr::RootAccess>,
) -> JsValue {
    let rendered = (|| {
        let ui = controller(plan.message)?;
        ui.locale();
        let owned = arguments(plan, proxy, root)?;
        let args = borrowed(&owned);
        ui.format(plan.message.id, &args)
    })();
    JsValue::from_str(&rendered.unwrap_or_else(|error| fallback(plan.message, &error)))
}

fn arguments(
    plan: &'static TranslationPlan,
    proxy: &JsValue,
    root: Option<&crate::expr::RootAccess>,
) -> Result<Vec<(&'static str, Argument)>, TranslationError> {
    if plan.arguments.len() != plan.message.arguments.len() {
        return Err(TranslationError::Initialization(
            "translation plan argument mismatch".into(),
        ));
    }
    // Evaluate every argument before validating any one value. A temporarily
    // invalid earlier value must not prevent tracking later reactive inputs.
    let values = plan
        .arguments
        .iter()
        .map(|expr| expr.evaluate_with(proxy, root))
        .collect::<Vec<_>>();
    plan.message
        .arguments
        .iter()
        .zip(values)
        .map(|((name, kind), value)| Argument::from_js(*kind, value).map(|value| (*name, value)))
        .collect()
}

fn borrowed<'a>(owned: &'a [(&'static str, Argument)]) -> Vec<(&'static str, Value<'a>)> {
    owned
        .iter()
        .map(|(name, value)| (*name, value.borrow()))
        .collect()
}

pub fn install(
    parent: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    plan: &'static TranslationPlan,
) {
    let children = parent.children();
    if children.length() as usize != plan.message.elements.len() {
        web_sys::console::error_1(
            &"$t placeholder elements changed before translation installation".into(),
        );
        return;
    }
    let elements = (0..children.length())
        .filter_map(|i| children.item(i))
        .collect::<Vec<_>>();
    let root = crate::scope::scoped_root_reader(scope_id);
    let proxy = proxy.clone();
    let target = parent.clone();
    let previous = Rc::new(RefCell::new(None));
    let id = effect(move || {
        let rendered = (|| {
            let ui = controller(plan.message)?;
            // Read language even when an argument is temporarily invalid, so
            // a later locale selection can recover the binding.
            ui.locale();
            let owned = arguments(plan, &proxy, root.as_ref())?;
            let args = borrowed(&owned);
            ui.render(plan.message.id, &args)
        })();
        match rendered {
            Ok(parts) => {
                if previous.borrow().as_ref() == Some(&parts) {
                    return;
                }
                if let Err(error) = render(&target, &elements, &parts) {
                    web_sys::console::error_2(&"$t DOM update failed".into(), &error);
                    return;
                }
                *previous.borrow_mut() = Some(parts);
            }
            Err(error) => {
                let text = fallback(plan.message, &error);
                // Never discard live placeholder elements on a diagnostic.
                // Plain messages can show their debug key without ownership loss.
                if elements.is_empty() {
                    target.set_text_content(Some(&text));
                }
                *previous.borrow_mut() = None;
            }
        }
    });
    track_effect_on(parent, id);
}

enum Argument {
    Text(String),
    Number(PluralArg),
    DateTime(DateTimeArg),
}
impl Argument {
    fn from_js(kind: ArgumentKind, value: JsValue) -> Result<Self, TranslationError> {
        let invalid =
            || TranslationError::Initialization(format!("translation argument must be {kind:?}"));
        match kind {
            ArgumentKind::Text => value.as_string().map(Self::Text).ok_or_else(invalid),
            ArgumentKind::Number => {
                // PluralArg's serde form is an exact decimal string, including
                // visible zeros. Ordinary JS numbers are accepted only while
                // their integer portion is safely representable.
                let text = if let Some(text) = value.as_string() {
                    text
                } else if let Some(number) = value.as_f64() {
                    if !number.is_finite() || number.abs() > 9_007_199_254_740_991.0 {
                        return Err(invalid());
                    }
                    crate::text::js_number_string(number)
                } else {
                    return Err(invalid());
                };
                text.parse().map(Self::Number).map_err(|_| invalid())
            }
            ArgumentKind::DateTime => serde_wasm_bindgen::from_value(value)
                .map(Self::DateTime)
                .map_err(|_| invalid()),
        }
    }
    fn borrow(&self) -> Value<'_> {
        match self {
            Self::Text(value) => Value::Text(value),
            Self::Number(value) => Value::Number(*value),
            Self::DateTime(value) => Value::DateTime(value),
        }
    }
}

fn fallback(message: CompiledMessage, error: &TranslationError) -> String {
    web_sys::console::error_1(&format!("translation {} failed: {error}", message.id.0).into());
    message
        .debug_key
        .map(|key| format!("⟦{key}⟧"))
        .unwrap_or_default()
}

/// Patch actual nodes, never translated HTML. All placeholders occur exactly
/// once on every branch. Place parents before descendants, then remove stale
/// text; moving an existing element preserves its listeners and scope state.
fn render(parent: &Element, elements: &[Element], parts: &[RenderedPart]) -> Result<(), JsValue> {
    let doc = parent
        .owner_document()
        .ok_or_else(|| JsValue::from_str("translation has no document"))?;
    let focused = doc.active_element().filter(|el| parent.contains(Some(el)));
    let mut parents = vec![parent.clone()];
    parents.extend_from_slice(elements);
    let mut children: Vec<Vec<Node>> = vec![Vec::new(); parents.len()];
    let mut stack = vec![0usize];
    let mut order = vec![0usize];
    for part in parts {
        let current = *stack.last().expect("root is always present");
        match part {
            RenderedPart::Text(text) => children[current].push(doc.create_text_node(text).into()),
            RenderedPart::OpenElement(index) => {
                let index = *index as usize + 1;
                let el = parents
                    .get(index)
                    .ok_or_else(|| JsValue::from_str("translation placeholder out of bounds"))?;
                children[current].push(el.clone().into());
                stack.push(index);
                order.push(index);
            }
            RenderedPart::CloseElement(_) => {
                stack.pop();
            }
        }
    }
    for &index in &order {
        let target = &parents[index];
        let mut anchor = target.first_child();
        for child in &children[index] {
            if anchor
                .as_ref()
                .is_some_and(|node| node.is_same_node(Some(child)))
            {
                anchor = child.next_sibling();
            } else {
                move_before(target, child, anchor.as_ref())?;
            }
        }
    }
    for &index in &order {
        let target = &parents[index];
        while target.child_nodes().length() as usize > children[index].len() {
            if let Some(last) = target.last_child() {
                target.remove_child(&last)?;
            }
        }
    }
    if let Some(focused) = focused
        && doc.active_element().as_ref() != Some(&focused)
        && let Ok(focus) =
            Reflect::get(&focused, &"focus".into()).and_then(|value| value.dyn_into::<Function>())
    {
        let options = js_sys::Object::new();
        Reflect::set(&options, &"preventScroll".into(), &JsValue::TRUE)?;
        let _ = focus.call1(&focused, &options);
    }
    Ok(())
}

fn move_before(parent: &Element, node: &Node, anchor: Option<&Node>) -> Result<(), JsValue> {
    // Use state-preserving moves when supplied by the browser. Detached mount
    // fragments and older engines retain the ordinary DOM insertion fallback.
    if let Ok(method) =
        Reflect::get(parent, &"moveBefore".into()).and_then(|value| value.dyn_into::<Function>())
    {
        let null = JsValue::NULL;
        let before = anchor.map(|node| node.as_ref()).unwrap_or(&null);
        if method.call2(parent, node, before).is_ok() {
            return Ok(());
        }
    }
    parent.insert_before(node, anchor).map(|_| ())
}
