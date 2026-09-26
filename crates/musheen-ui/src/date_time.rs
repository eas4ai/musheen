use crate::i18n::Locale;
use jiff::tz::TimeZone;
use std::sync::OnceLock;

static LOCAL_TIME_ZONE: OnceLock<TimeZone> = OnceLock::new();

pub(crate) fn format_modified(unix_seconds: i64, locale: Locale) -> String {
    let time_zone = LOCAL_TIME_ZONE.get_or_init(TimeZone::system);
    format_modified_in_zone(unix_seconds, locale, time_zone)
}

fn format_modified_in_zone(unix_seconds: i64, locale: Locale, time_zone: &TimeZone) -> String {
    let Ok(timestamp) = jiff::Timestamp::new(unix_seconds, 0) else {
        return format!("{unix_seconds} Unix seconds");
    };
    let local = timestamp.to_zoned(time_zone.clone());
    let year = local.year();
    let month = local.month();
    let day = local.day();
    let hour = local.hour();
    let minute = local.minute();
    let zone = local.strftime("%Z");
    let value = match locale {
        Locale::EnUs => format!("{month:02}/{day:02}/{year:04}, {hour:02}:{minute:02} {zone}"),
        Locale::EnXa => {
            format!("⟦{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} {zone}⟧")
        }
        Locale::Ar => format!("{year:04}/{month:02}/{day:02} {hour:02}:{minute:02} {zone}"),
    };
    if locale == Locale::Ar {
        localize_arabic_digits(value)
    } else {
        value
    }
}

fn localize_arabic_digits(value: String) -> String {
    value
        .chars()
        .map(|character| match character {
            '0'..='9' => char::from_u32('٠' as u32 + character as u32 - '0' as u32)
                .expect("Arabic decimal digit"),
            _ => character,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modified_dates_use_the_local_time_zone_and_locale() {
        let eastern = TimeZone::get("America/New_York").unwrap();

        assert_eq!(
            format_modified_in_zone(0, Locale::EnUs, &eastern),
            "12/31/1969, 19:00 EST"
        );
        assert_eq!(
            format_modified_in_zone(0, Locale::EnXa, &eastern),
            "⟦1969-12-31 19:00 EST⟧"
        );
        assert_eq!(
            format_modified_in_zone(0, Locale::Ar, &eastern),
            "١٩٦٩/١٢/٣١ ١٩:٠٠ EST"
        );
        assert_eq!(
            format_modified_in_zone(1_719_792_000, Locale::EnUs, &eastern),
            "06/30/2024, 20:00 EDT"
        );
    }
}
