use super::*;

impl Formatter {
    pub(crate) fn with_provider(
        provider: &impl icu_provider::buf::BufferProvider,
        locale: &Locale,
    ) -> Result<Self, RenderError> {
        let locale: icu_locale_core::Locale = locale.as_str().parse().map_err(error)?;
        let decimal = DecimalFormatter::try_new_with_buffer_provider(
            provider,
            locale.clone().into(),
            Default::default(),
        )
        .map_err(error)?;
        let percent = PercentFormatter::try_new_with_buffer_provider(
            provider,
            locale.clone().into(),
            Default::default(),
        )
        .map_err(error)?;
        let dates = [
            fieldsets::YMD::short(),
            fieldsets::YMD::medium(),
            fieldsets::YMD::long(),
        ]
        .map(|fields| {
            DateTimeFormatter::try_new_with_buffer_provider(provider, locale.clone().into(), fields)
                .map_err(error)
        });
        let [short, medium, long] = dates;
        let times = [fieldsets::T::hm(), fieldsets::T::hms()].map(|fields| {
            NoCalendarFormatter::try_new_with_buffer_provider(
                provider,
                locale.clone().into(),
                fields,
            )
            .map_err(error)
        });
        let [hm, hms] = times;
        Ok(Self {
            decimal,
            percent,
            dates: [short?, medium?, long?],
            times: [hm?, hms?],
        })
    }
}
