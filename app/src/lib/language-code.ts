/** Canonical BCP 47 tags, including script + region (sr-Latn-RS), variants and numeric regions. */
export function isLanguageCode(code: string): boolean {
  try {
    return new Intl.Locale(code).toString() === code;
  } catch {
    return false;
  }
}
