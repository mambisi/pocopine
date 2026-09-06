use js_sys::{Array, Function, Object, Reflect};
use std::{cell::RefCell, collections::VecDeque};
use wasm_bindgen::JsValue;

use crate::{DateTimeArg, DateTimeStyle, Locale, NumberStyle, PluralArg, RenderError, StyleLength};

pub(crate) struct Formatter {
    locale: String,
    decimal: Function,
    percent: Function,
    dates: RefCell<VecDeque<(String, DateTimeStyle, Function)>>,
}

fn error(value: JsValue) -> RenderError {
    RenderError::Formatter(value.as_string().unwrap_or_else(|| format!("{value:?}")))
}
fn set(options: &Object, key: &str, value: impl Into<JsValue>) -> Result<(), RenderError> {
    Reflect::set(options, &JsValue::from_str(key), &value.into()).map_err(error)?;
    Ok(())
}
fn locales(locale: &str) -> Array {
    let values = Array::new();
    values.push(&JsValue::from_str(locale));
    // An ICU-unsupported locale must not silently use the browser user's
    // ambient language. English is the explicit root formatting fallback.
    if locale != "en" {
        values.push(&JsValue::from_str("en"));
    }
    values
}

impl Formatter {
    pub(crate) fn new(locale: &Locale) -> Result<Self, RenderError> {
        let decimal = Object::new();
        set(&decimal, "maximumFractionDigits", 3)?;
        let percent = Object::new();
        set(&percent, "style", "percent")?;
        set(&percent, "maximumFractionDigits", 0)?;
        Ok(Self {
            locale: locale.as_str().into(),
            decimal: construct("NumberFormat", locale.as_str(), &decimal)?,
            percent: construct("NumberFormat", locale.as_str(), &percent)?,
            dates: RefCell::new(VecDeque::new()),
        })
    }
    pub(crate) fn number(
        &self,
        value: PluralArg,
        style: NumberStyle,
    ) -> Result<String, RenderError> {
        // ECMA-402 ToIntlMathematicalValue accepts an exact decimal string.
        // A JS Number here would round u64 and visible-fraction inputs first.
        let formatter = match style {
            NumberStyle::Decimal => &self.decimal,
            NumberStyle::Percent => &self.percent,
        };
        formatter
            .call1(&JsValue::UNDEFINED, &JsValue::from_str(&value.to_string()))
            .map_err(error)?
            .as_string()
            .ok_or_else(|| RenderError::Formatter("Intl returned non-string number text".into()))
    }
    pub(crate) fn datetime(
        &self,
        value: &DateTimeArg,
        style: DateTimeStyle,
    ) -> Result<String, RenderError> {
        let cached = self
            .dates
            .borrow()
            .iter()
            .find_map(|(zone, cached_style, format)| {
                (zone == value.time_zone().as_str() && *cached_style == style)
                    .then(|| format.clone())
            });
        let formatter = if let Some(formatter) = cached {
            formatter
        } else {
            let formatter = self.date_formatter(value, style)?;
            // Recipient zones are inputs, so bound the cache independently of
            // how many distinct zones an application visits over its lifetime.
            let mut dates = self.dates.borrow_mut();
            if dates.len() == 32 {
                dates.pop_front();
            }
            dates.push_back((value.time_zone().as_str().into(), style, formatter.clone()));
            formatter
        };
        formatter
            .call1(
                &JsValue::UNDEFINED,
                &JsValue::from_f64(value.unix_millis() as f64),
            )
            .map_err(error)?
            .as_string()
            .ok_or_else(|| RenderError::Formatter("Intl returned non-string date text".into()))
    }

    fn date_formatter(
        &self,
        value: &DateTimeArg,
        style: DateTimeStyle,
    ) -> Result<Function, RenderError> {
        let options = Object::new();
        set(&options, "timeZone", value.time_zone().as_str())?;
        let (kind, length) = match style {
            DateTimeStyle::Date(length) => ("dateStyle", length),
            DateTimeStyle::Time(length) => ("timeStyle", length),
        };
        set(
            &options,
            kind,
            match length {
                StyleLength::Short => "short",
                StyleLength::Medium => "medium",
                StyleLength::Long => "long",
            },
        )?;
        // Construct with Reflect to catch RangeError from unavailable zones.
        construct("DateTimeFormat", &self.locale, &options)
    }
}

fn construct(kind: &str, locale: &str, options: &Object) -> Result<Function, RenderError> {
    use wasm_bindgen::JsCast;
    let intl = Reflect::get(&js_sys::global(), &JsValue::from_str("Intl")).map_err(error)?;
    let constructor = Reflect::get(&intl, &JsValue::from_str(kind))
        .map_err(error)?
        .dyn_into::<Function>()
        .map_err(error)?;
    let args = Array::new();
    args.push(&locales(locale));
    args.push(options);
    let formatter = Reflect::construct(&constructor, &args).map_err(error)?;
    Reflect::get(&formatter, &JsValue::from_str("format"))
        .map_err(error)?
        .dyn_into::<Function>()
        .map_err(error)
}

pub(crate) fn validate_time_zone(name: &str) -> Result<(), RenderError> {
    let options = Object::new();
    set(&options, "timeZone", name)?;
    construct("DateTimeFormat", "en", &options)
        .map(|_| ())
        .map_err(|_| RenderError::InvalidTimeZone(name.into()))
}
