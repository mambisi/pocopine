//! Fixed ICU4X backend: always on the host, opt-in on wasm for strict parity.

use fixed_decimal::{Decimal, SignedRoundingMode, UnsignedRoundingMode};
use icu_datetime::{
    DateTimeFormatter, NoCalendarFormatter, fieldsets,
    input::{Date, Time},
};
use icu_decimal::DecimalFormatter;
use icu_experimental::dimension::percent::formatter::PercentFormatter;

use crate::{DateTimeArg, DateTimeStyle, Locale, NumberStyle, PluralArg, RenderError, StyleLength};

pub(crate) struct Formatter {
    decimal: DecimalFormatter,
    percent: PercentFormatter<DecimalFormatter>,
    dates: [DateTimeFormatter<fieldsets::YMD>; 3],
    times: [NoCalendarFormatter<fieldsets::T>; 2],
}

fn error(value: impl std::fmt::Display) -> RenderError {
    RenderError::Formatter(value.to_string())
}

impl Formatter {
    pub(crate) fn new(locale: &Locale) -> Result<Self, RenderError> {
        let locale: icu_locale_core::Locale = locale.as_str().parse().map_err(error)?;
        let decimal =
            DecimalFormatter::try_new(locale.clone().into(), Default::default()).map_err(error)?;
        let percent =
            PercentFormatter::try_new(locale.clone().into(), Default::default()).map_err(error)?;
        let dates = [
            fieldsets::YMD::short(),
            fieldsets::YMD::medium(),
            fieldsets::YMD::long(),
        ]
        .map(|fields| DateTimeFormatter::try_new(locale.clone().into(), fields).map_err(error));
        let [short, medium, long] = dates;
        let times = [fieldsets::T::hm(), fieldsets::T::hms()].map(|fields| {
            NoCalendarFormatter::try_new(locale.clone().into(), fields).map_err(error)
        });
        let [hm, hms] = times;
        Ok(Self {
            decimal,
            percent,
            dates: [short?, medium?, long?],
            times: [hm?, hms?],
        })
    }

    pub(crate) fn number(
        &self,
        value: PluralArg,
        style: NumberStyle,
    ) -> Result<String, RenderError> {
        let mut decimal: Decimal = value.to_string().parse().map_err(error)?;
        // Match Intl's ordinary decimal/percent precision defaults without
        // converting exact operands through binary floating point.
        let precision = match style {
            NumberStyle::Decimal => -3,
            NumberStyle::Percent => {
                decimal.multiply_pow10(2);
                0
            }
        };
        decimal.round_with_mode(
            precision,
            SignedRoundingMode::Unsigned(UnsignedRoundingMode::HalfExpand),
        );
        decimal.trim_end();
        decimal.trim_start();
        Ok(match style {
            NumberStyle::Decimal => self.decimal.format(&decimal).to_string(),
            NumberStyle::Percent => self.percent.format(&decimal).to_string(),
        })
    }

    pub(crate) fn datetime(
        &self,
        value: &DateTimeArg,
        style: DateTimeStyle,
    ) -> Result<String, RenderError> {
        let zone = jiff::tz::TimeZoneDatabase::bundled()
            .get(value.time_zone().as_str())
            .map_err(error)?;
        let zoned = jiff::Timestamp::from_millisecond(value.unix_millis())
            .map_err(error)?
            .to_zoned(zone);
        match style {
            DateTimeStyle::Date(length) => {
                let date = Date::try_new_iso(
                    i32::from(zoned.year()),
                    zoned.month() as u8,
                    zoned.day() as u8,
                )
                .map_err(error)?;
                let index = match length {
                    StyleLength::Short => 0,
                    StyleLength::Medium => 1,
                    StyleLength::Long => 2,
                };
                Ok(self.dates[index].format(&date).to_string())
            }
            DateTimeStyle::Time(length) => {
                let time = Time::try_new(
                    zoned.hour() as u8,
                    zoned.minute() as u8,
                    zoned.second() as u8,
                    0,
                )
                .map_err(error)?;
                let index = match length {
                    StyleLength::Short => 0,
                    StyleLength::Medium => 1,
                    StyleLength::Long => {
                        return Err(error("long time style is outside the MF1 subset"));
                    }
                };
                Ok(self.times[index].format(&time).to_string())
            }
        }
    }
}

pub(crate) fn validate_time_zone(name: &str) -> Result<(), RenderError> {
    jiff::tz::TimeZoneDatabase::bundled()
        .get(name)
        .map(|_| ())
        .map_err(|_| RenderError::InvalidTimeZone(name.into()))
}
