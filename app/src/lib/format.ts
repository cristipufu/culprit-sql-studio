import type { Unit } from "../gen/Unit";
import { language, locale, t } from "./i18n";

// Every number here reads as culprit_core::format writes it in findings and briefs, so a value reads the same on
// every surface. `bun run test` compares them on the cases Rust writes to gen/format-cases.json (format.test.ts).
// Digits take the app's language's separators, and units and time words are its messages; English reads as Rust's.

/** `x.toFixed(digits)` rounded as Rust's `format!("{:.N}")` rounds: an exact tie goes to the even digit (2.5 → "2",
 * 0.125 → "0.12", 12.5% → "12%"), where toFixed rounds it up. Exact ties are the values that are an odd number of
 * 1/2^(digits+1); every other value rounds the same in both. */
function fixed(x: number, digits: number): string {
  const text = x.toFixed(digits);
  const halves = Math.abs(x) * 2 ** (digits + 1);
  if (!Number.isInteger(halves) || halves % 2 !== 1) return text;
  const scale = 10 ** digits;
  const up = Math.round(Math.abs(Number(text)) * scale);
  if (up % 2 === 0) return text;
  return `${x < 0 ? "-" : ""}${((up - 1) / scale).toFixed(digits)}`;
}

/** Rust's `f64::round`: half away from zero (`Math.round` rounds -2.5 to -2). */
function roundHalfAway(x: number): number {
  return Math.sign(x) * Math.round(Math.abs(x));
}

/** Digits written the English way ("1234.5", "1,234.5") in the app's language's separators ("1234,5", "1.234,5"). */
export function digits(text: string): string {
  const { decimal, group } = locale();
  if (decimal === "." && group === ",") return text;
  return text.replace(/[.,]/g, (c) => (c === "." ? decimal : group));
}

/** The locale dates and times are written in: the system's own where it speaks the app's language (so en-GB keeps
 * "7 Oct"), else the app's language. */
export function dateLocale(): string {
  const code = language();
  const base = code.split("-")[0];
  const system = typeof navigator === "undefined" ? [] : (navigator.languages ?? [navigator.language]);
  return system.find((tag) => tag && (tag === code || tag.split("-")[0] === base)) ?? code;
}

const dateFormats = new Map<string, Intl.DateTimeFormat>();

/** A date and time formatter in the app's language ({@link dateLocale}), made once per language and options. */
export function dateFormat(options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  const locale = dateLocale();
  const key = `${locale} ${JSON.stringify(options)}`;
  let format = dateFormats.get(key);
  if (!format) {
    format = new Intl.DateTimeFormat(locale, options);
    dateFormats.set(key, format);
  }
  return format;
}

export function duration(ms: number | null | undefined): string {
  if (ms == null) return "—";
  const v = Math.max(0, ms);
  if (v === 0) return t("ui.format.duration.ms", { value: "0" });
  // Under a hundredth of a millisecond, "0.00 ms" says nothing: microseconds do.
  if (v < 0.01) return t("ui.format.duration.us", { value: digits(fixed(Math.max(1, v * 1000), 0)) });
  if (v < 1) return t("ui.format.duration.ms", { value: digits(fixed(v, 2)) });
  // A tenth under 10 ms: rounding 2.59 ms to "3 ms" is off by 16%. A whole number reads without its ".0".
  if (v < 9.95) {
    const tenths = fixed(v, 1);
    return t("ui.format.duration.ms", { value: digits(tenths.endsWith(".0") ? tenths.slice(0, -2) : tenths) });
  }
  if (v < 1000) return t("ui.format.duration.ms", { value: digits(fixed(v, 0)) });
  const s = v / 1000;
  if (s < 60) return t("ui.format.duration.s", { value: digits(fixed(s, 1)) });
  const m = s / 60;
  if (m < 60) return t("ui.format.duration.min", { value: digits(fixed(m, 0)) });
  const h = m / 60;
  if (h < 48) {
    const whole = Math.floor(h);
    const minutes = roundHalfAway((h - whole) * 60);
    return minutes >= 1 ? t("ui.format.duration.h_min", { hours: whole, minutes }) : t("ui.format.duration.h", { hours: whole });
  }
  return dayCount(fixed(h / 24, 1));
}

/** "1 day", "9.5 days", from a number of days in English digits. */
function dayCount(english: string): string {
  return t("ui.format.days", { n: Number(english), count: digits(english) });
}

/** Days counted, never more than were: a tenth under 10 (9.51 days are "9.5", not "10"), whole days from 10 on.
 * Mirrors culprit_core::format::days_counted. */
export function daysCounted(value: number): string {
  return digits(countedDays(value));
}

/** `daysCounted` in English digits. */
function countedDays(value: number): string {
  // A hair under a whole number from adding hours is that number.
  const d = value + 1e-9;
  if (d < 10) {
    const tenths = fixed(Math.floor(d * 10) / 10, 1);
    return tenths.endsWith(".0") ? tenths.slice(0, -2) : tenths;
  }
  return `${Math.floor(d)}`;
}

/** A span of days as a metric says it: "9.5 days", "1 day", and under a day hours and minutes ("9 h 36 min"), never
 * "0 days". Mirrors culprit_core::format::days. */
export function days(value: number): string {
  if (value < 1) return duration(Math.max(0, value) * 86_400_000);
  return dayCount(countedDays(value));
}

export function bytes(value: number | null | undefined): string {
  if (value == null) return "—";
  let v = Math.max(0, value);
  let unit = 0;
  while (v >= 1024 && unit < 4) {
    v /= 1024;
    unit++;
  }
  return byteSize(unit, digits(unit === 0 || v >= 100 ? fixed(v, 0) : fixed(v, 1)));
}

/** `value` in the `unit`th of B, KB, MB, GB and TB. */
function byteSize(unit: number, value: string): string {
  switch (unit) {
    case 0:
      return t("ui.format.bytes.b", { value });
    case 1:
      return t("ui.format.bytes.kb", { value });
    case 2:
      return t("ui.format.bytes.mb", { value });
    case 3:
      return t("ui.format.bytes.gb", { value });
    default:
      return t("ui.format.bytes.tb", { value });
  }
}

export function count(value: number | null | undefined): string {
  if (value == null) return "—";
  // An empty sum is -0; it reads as nothing, not as a negative.
  const n = value === 0 ? 0 : value;
  const v = Math.abs(n);
  if (v >= 1e9) return t("ui.format.count.billions", { value: digits(fixed(n / 1e9, 1)) });
  if (v >= 1e6) return t("ui.format.count.millions", { value: digits(fixed(n / 1e6, 1)) });
  if (v >= 1e4) return t("ui.format.count.thousands", { value: digits(fixed(n / 1e3, 0)) });
  if (v >= 1e3) return t("ui.format.count.thousands", { value: digits(fixed(n / 1e3, 1)) });
  if (v >= 100 || Number.isInteger(n)) return digits(fixed(n, 0));
  return digits(fixed(n, 1));
}

/** "1 row", "12 rows", "1.2K rows"; "— rows" when unknown. */
export function rowCount(value: number | null | undefined): string {
  return t("ui.format.row_count", { n: value ?? Number.NaN, count: count(value) });
}

export function percent(ratio: number | null | undefined): string {
  if (ratio == null) return "—";
  const p = ratio * 100;
  if (p > 0 && p < 1) return t("ui.format.percent.under", { value: digits("1") });
  return t("ui.format.percent.value", { value: digits(fixed(p, 0)) });
}

/** A measured percentage on a 0–100 scale. Mirrors culprit_core::format::percent_value. */
export function percentValue(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "—";
  if (value === 0) return t("ui.format.percent.value", { value: "0" });
  if (Math.abs(value) < 0.01) {
    return value > 0 ? t("ui.format.percent.under", { value: digits("0.01") }) : t("ui.format.percent.over", { value: digits("-0.01") });
  }
  return t("ui.format.percent.value", { value: digits(fixed(value, 2).replace(/0+$/, "").replace(/\.$/, "")) });
}

/** A total wait to the second under an hour: "1 min 29 s", where `duration` would round 88,960 ms down to "1 min" next to
 * a title that says otherwise, and 152 s up to "3 min". Mirrors culprit_checks::signals::total_wait. */
export function totalWait(ms: number): string {
  const seconds = Math.round(ms / 1000);
  if (seconds < 60 || seconds >= 3600) return duration(ms);
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return rest === 0 ? t("ui.format.duration.min", { value: String(minutes) }) : t("ui.format.duration.min_s", { minutes, seconds: rest });
}

/** Metrics that total a wait, said to the second as their findings' titles say them. Mirrors
 * culprit_checks::TOTAL_WAIT_METRICS. */
const TOTAL_WAIT_METRICS = new Set(["Time waiting", "Time waiting since start"]);

/** A finding's metric as its Evidence says it. Mirrors culprit_checks::metric_text. */
export function findingMetric(m: { label: string; value: number; unit: Unit; text?: string | null }): string {
  return m.unit === "ms" && TOTAL_WAIT_METRICS.has(m.label) ? totalWait(m.value) : metric(m.value, m.unit, m.text);
}

export function metric(value: number, unit: Unit, text?: string | null): string {
  switch (unit) {
    case "ms":
      return duration(value);
    case "bytes":
      return bytes(value);
    case "ratio":
      return percent(value);
    case "days":
      return days(value);
    case "text":
      return text ?? "";
    default:
      return count(value);
  }
}

export function ago(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return t("ui.format.ago.never");
  const at = Date.parse(iso);
  if (Number.isNaN(at)) return iso;
  const s = Math.max(0, (now - at) / 1000);
  if (s < 45) return t("ui.format.ago.just_now");
  if (s < 3600) return t("ui.format.ago.minutes", { n: Math.round(s / 60) });
  if (s < 86400) return t("ui.format.ago.hours", { n: Math.round(s / 3600) });
  const n = Math.round(s / 86400);
  return t("ui.format.ago.days", { n, count: String(n) });
}

/** A moment as people say it: "today 12:13", "yesterday 09:02", "Oct 7, 12:13", "Oct 7, 2025". `full` is for tooltips. */
export function when(iso: string | null | undefined, now = new Date()): { short: string; full: string } {
  if (!iso) return { short: "", full: "" };
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return { short: iso, full: iso };
  const locale = dateLocale();
  const time = d.toLocaleTimeString(locale, { hour: "numeric", minute: "2-digit" });
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((day(now) - day(d)) / 86_400_000);
  const short =
    days === 0
      ? t("ui.format.when.today", { time })
      : days === 1
        ? t("ui.format.when.yesterday", { time })
        : d.getFullYear() === now.getFullYear()
          ? t("ui.format.when.this_year", { date: d.toLocaleDateString(locale, { month: "short", day: "numeric" }), time })
          : d.toLocaleDateString(locale, { month: "short", day: "numeric", year: "numeric" });
  return { short, full: d.toLocaleString(locale, { dateStyle: "full", timeStyle: "short" }) };
}

/** A period's length in the unit it reads best in, rounded. */
function periodLength(hours: number): { unit: "day" | "hour" | "minute"; args: { n: number; count: string } } {
  const [unit, n] = hours >= 47 ? (["day", Math.round(hours / 24)] as const)
    : hours >= 1.5 ? (["hour", Math.round(hours)] as const)
    : (["minute", Math.max(1, Math.round(hours * 60))] as const);
  return { unit, args: { n, count: String(n) } };
}

/** A recent period by its length: "last 7 days", "last 5 hours", "last 35 minutes". */
export function windowLabel(hours: number): string {
  const { unit, args } = periodLength(hours);
  if (unit === "day") return t("ui.format.window.days", args);
  if (unit === "hour") return t("ui.format.window.hours", args);
  return t("ui.format.window.minutes", args);
}

/** How long a period is, rounded as `windowLabel` rounds it: "7 days", "5 hours", "35 minutes". */
export function windowSpan(hours: number): string {
  const { unit, args } = periodLength(hours);
  if (unit === "day") return t("ui.format.span.days", args);
  if (unit === "hour") return t("ui.format.span.hours", args);
  return t("ui.format.span.minutes", args);
}

export function oneLine(sql: string): string {
  return sql.replace(/\s+/g, " ").trim();
}
