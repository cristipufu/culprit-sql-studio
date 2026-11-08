import cardinals from "../../../locales/.cldr/cardinals.json";

export type PluralCategory = "zero" | "one" | "two" | "few" | "many" | "other";
type Relation = [string, number, boolean, [number, number][]];
type PluralCase = [PluralCategory, Relation[][]];
const languages = cardinals.languages as Record<string, number>;
const rules = cardinals.rules as PluralCase[][];

/** Resolve regional/script variants through the CLDR parent language. Unknown rules are a catalog error. */
export function pluralRule(code: string): number | undefined {
  let tag = code;
  for (;;) {
    const index = languages[tag];
    if (index !== undefined) return index;
    const dash = tag.lastIndexOf("-");
    if (dash < 0) return undefined;
    tag = tag.slice(0, dash);
  }
}

function operands(number: number): Record<string, number> {
  const [mantissa = "", exponent = "0"] = String(number).split("e");
  const whole = mantissa.includes(".") ? mantissa.indexOf(".") : mantissa.length;
  const digits = mantissa.replace(".", "");
  const decimal = whole + Number(exponent);
  const length = Math.max(0, digits.length - decimal);
  const fraction = length === 0 ? 0 : Number(digits.slice(Math.max(0, decimal)));
  return { n: number, i: Math.floor(number), v: length, w: length, f: fraction, t: fraction, c: 0, e: 0 };
}

/** Numeric arguments have no visible trailing zeroes and no compact-number exponent. */
export function cardinalCategory(number: number, code: string): PluralCategory {
  const index = pluralRule(code);
  if (index === undefined) throw new Error(`Unknown CLDR plural rule ${code}`);
  if (!Number.isFinite(number)) return "other";
  const values = operands(Math.abs(number));
  const matches = ([operand, modulus, negate, ranges]: Relation) => {
    const value = modulus ? values[operand]! % modulus : values[operand]!;
    return (Number.isInteger(value) && ranges.some(([start, end]) => value >= start && value <= end)) !== negate;
  };
  return rules[index]!.find(([, alternatives]) => alternatives.some((terms) => terms.every(matches)))?.[0] ?? "other";
}
