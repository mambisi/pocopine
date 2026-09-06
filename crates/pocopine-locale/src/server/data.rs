//! Pre-resolve only the ICU payloads requested by our exact formatter set.
//! The recorder uses the pinned baked providers' own fallback; exported keys
//! retain each requested locale/attribute, so runtime needs no fallback tables.

mod plurals;
pub use plurals::plural_data;

use crate::Locales;
use icu_provider::{
    buf::{BufferFormat, BufferMarker},
    dynutil::UpcastDataPayload,
    export::{DataExporter, ExportMarker},
    prelude::*,
};
use icu_provider_blob::{BlobDataProvider, export::BlobExporter};
use std::cell::RefCell;

/// Generate deterministic, configured-locale ICU data. It is compiled into the
/// host and strict-parity browser; the default browser keeps using Intl.
pub fn formatting_data(locales: &Locales) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    {
        let mut exporter = BlobExporter::new_with_sink(Box::new(&mut bytes));
        let recorder = Recorder {
            exporter: &exporter,
            markers: RefCell::new(Vec::new()),
        };
        for locale in locales.supported() {
            crate::icu::Formatter::with_provider(&recorder, locale)
                .map_err(|e| format!("ICU data for {locale}: {e}"))?;
        }
        for marker in recorder.markers.into_inner() {
            exporter
                .flush(marker, Default::default())
                .map_err(|e| e.to_string())?;
        }
        exporter.close().map_err(|e| e.to_string())?;
    }
    // Replay with the actual runtime provider before publishing a partial pack.
    let provider = BlobDataProvider::try_new_from_blob(bytes.clone().into_boxed_slice())
        .map_err(|e| e.to_string())?;
    for locale in locales.supported() {
        crate::icu::Formatter::with_provider(&provider, locale)
            .map_err(|e| format!("sliced ICU data for {locale}: {e}"))?;
    }
    Ok(bytes)
}

struct Recorder<'a> {
    exporter: &'a dyn DataExporter,
    markers: RefCell<Vec<DataMarkerInfo>>,
}

impl Recorder<'_> {
    fn capture<M: DataMarker>(
        &self,
        provider: &impl DataProvider<M>,
        req: DataRequest,
    ) -> Result<DataResponse<BufferMarker>, DataError>
    where
        ExportMarker: UpcastDataPayload<M>,
    {
        let response = provider.load(req)?;
        let payload: DataPayload<ExportMarker> = UpcastDataPayload::upcast(response.payload);
        self.exporter.put_payload(M::INFO, req.id, &payload)?;
        if !self
            .markers
            .borrow()
            .iter()
            .any(|marker| marker.id == M::INFO.id)
        {
            self.markers.borrow_mut().push(M::INFO);
        }
        let mut bytes = Vec::new();
        payload.serialize(&mut serde_json::Serializer::new(&mut bytes))?;
        let mut metadata = response.metadata;
        metadata.buffer_format = Some(BufferFormat::Json);
        Ok(DataResponse {
            metadata,
            payload: DataPayload::from_owned_buffer(bytes.into_boxed_slice()),
        })
    }
}

impl DynamicDataProvider<BufferMarker> for Recorder<'_> {
    fn load_data(
        &self,
        marker: DataMarkerInfo,
        req: DataRequest,
    ) -> Result<DataResponse<BufferMarker>, DataError> {
        macro_rules! markers {
            ($provider:path; $($marker:path),* $(,)?) => {$ (
                if marker.id == <$marker>::INFO.id { return self.capture::<$marker>(&$provider, req); }
            )*};
        }
        markers!(icu_decimal::provider::Baked;
            icu_decimal::provider::DecimalSymbolsV1,
            icu_decimal::provider::DecimalDigitsV1,
        );
        markers!(icu_datetime::provider::Baked;
            icu_datetime::provider::names::DatetimeNamesWeekdayV1,
            icu_datetime::provider::names::DatetimeNamesDayperiodV1,
            icu_datetime::provider::names::DatetimeNamesYearBuddhistV1,
            icu_datetime::provider::names::DatetimeNamesYearChineseV1,
            icu_datetime::provider::names::DatetimeNamesYearCopticV1,
            icu_datetime::provider::names::DatetimeNamesYearDangiV1,
            icu_datetime::provider::names::DatetimeNamesYearEthiopianV1,
            icu_datetime::provider::names::DatetimeNamesYearGregorianV1,
            icu_datetime::provider::names::DatetimeNamesYearHebrewV1,
            icu_datetime::provider::names::DatetimeNamesYearIndianV1,
            icu_datetime::provider::names::DatetimeNamesYearHijriV1,
            icu_datetime::provider::names::DatetimeNamesYearJapaneseV1,
            icu_datetime::provider::names::DatetimeNamesYearPersianV1,
            icu_datetime::provider::names::DatetimeNamesYearRocV1,
            icu_datetime::provider::names::DatetimeNamesMonthBuddhistV1,
            icu_datetime::provider::names::DatetimeNamesMonthChineseV1,
            icu_datetime::provider::names::DatetimeNamesMonthCopticV1,
            icu_datetime::provider::names::DatetimeNamesMonthDangiV1,
            icu_datetime::provider::names::DatetimeNamesMonthEthiopianV1,
            icu_datetime::provider::names::DatetimeNamesMonthGregorianV1,
            icu_datetime::provider::names::DatetimeNamesMonthHebrewV1,
            icu_datetime::provider::names::DatetimeNamesMonthIndianV1,
            icu_datetime::provider::names::DatetimeNamesMonthHijriV1,
            icu_datetime::provider::names::DatetimeNamesMonthJapaneseV1,
            icu_datetime::provider::names::DatetimeNamesMonthPersianV1,
            icu_datetime::provider::names::DatetimeNamesMonthRocV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsGlueV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsTimeV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsDateBuddhistV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsDateChineseV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsDateCopticV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsDateDangiV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsDateEthiopianV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsDateGregorianV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsDateHebrewV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsDateIndianV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsDateHijriV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsDateJapaneseV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsDatePersianV1,
            icu_datetime::provider::semantic_skeletons::DatetimePatternsDateRocV1,
        );
        markers!(icu_calendar::provider::Baked;
            icu_calendar::provider::CalendarJapaneseModernV1,
            icu_calendar::provider::CalendarWeekV1,
            icu_calendar::provider::CalendarPreferredV1,
        );
        markers!(icu_plurals::provider::Baked;
            icu_plurals::provider::PluralsCardinalV1,
            icu_plurals::provider::PluralsOrdinalV1,
        );
        markers!(icu_time::provider::Baked;
            icu_time::provider::iana::TimezoneIdentifiersIanaExtendedV1,
            icu_time::provider::iana::TimezoneIdentifiersIanaCoreV1,
            icu_time::provider::windows::TimezoneIdentifiersWindowsV1,
            icu_time::provider::TimezonePeriodsV1,
        );
        markers!(icu_experimental::provider::Baked;
            icu_experimental::dimension::provider::percent::PercentEssentialsV1,
        );
        Err(DataErrorKind::MarkerNotFound.with_req(marker, req))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DateTimeArg, DateTimeStyle, NumberStyle, StyleLength, TimeZone};

    fn locales(tags: &[&str]) -> Locales {
        Locales::new(
            tags[0].parse().unwrap(),
            tags.iter().map(|s| s.parse().unwrap()),
        )
        .unwrap()
    }

    #[test]
    fn sliced_data_matches_baked_formatters_and_grows_only_with_requested_locales() {
        let one = formatting_data(&locales(&["en"])).unwrap();
        let config = locales(&["en", "fr", "ar", "th", "fa", "ja", "zh-Hant", "es-AR"]);
        let bytes = formatting_data(&config).unwrap();
        assert_eq!(bytes, formatting_data(&config).unwrap());
        assert!(one.len() < bytes.len());
        let provider = BlobDataProvider::try_new_from_blob(bytes.into_boxed_slice()).unwrap();
        for locale in config.supported() {
            let baked = crate::icu::Formatter::new(locale).unwrap();
            let sliced = crate::icu::Formatter::with_provider(&provider, locale).unwrap();
            for value in ["0", "1", "12345.67", "-0.125"] {
                for style in [NumberStyle::Decimal, NumberStyle::Percent] {
                    assert_eq!(
                        baked.number(value.parse().unwrap(), style),
                        sliced.number(value.parse().unwrap(), style),
                        "{locale}/{value}"
                    );
                }
            }
            let date = DateTimeArg::new(1720000000000, TimeZone::utc()).unwrap();
            for style in [
                DateTimeStyle::Date(StyleLength::Short),
                DateTimeStyle::Date(StyleLength::Medium),
                DateTimeStyle::Date(StyleLength::Long),
                DateTimeStyle::Time(StyleLength::Short),
                DateTimeStyle::Time(StyleLength::Medium),
                DateTimeStyle::Time(StyleLength::Long),
            ] {
                assert_eq!(
                    baked.datetime(&date, style),
                    sliced.datetime(&date, style),
                    "{locale}/{style:?}"
                );
            }
        }
        assert!(crate::icu::Formatter::with_provider(&provider, &"de".parse().unwrap()).is_err());
    }
}
