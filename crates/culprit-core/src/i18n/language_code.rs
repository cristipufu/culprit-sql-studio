/// Catalog filenames use structurally canonical BCP 47 tags, including scripts, regions and variants.
pub fn is_language_code(code: &str) -> bool {
    icu_locale_core::Locale::try_from_str(code).is_ok_and(|locale| locale.to_string() == code)
}
