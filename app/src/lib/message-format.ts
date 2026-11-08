// The message syntax every catalog in locales/*.json uses: a subset of ICU MessageFormat, the format translation tools
// already know, implemented the same way here and in Rust (culprit_core::i18n), checked by the shared test cases in
// locales/.tests/messages.json.
//
//   {name}                                           an argument: text as given, a number in the language's digits
//   {n, plural, =0 {none} one {# row} other {# rows}}  a case by the language's plural rule; # is n in its digits
//   {v, select, past {took} other {takes}}           a case by the argument's text; `other` is required
//
// Braces are never literal text: SQL and other text with braces goes in an argument.
import { cardinalCategory, type PluralCategory } from "./plural";

/** How a language counts and writes numbers: from its catalog's `_meta`. */
export type Locale = {
  /** A CLDR language tag selecting the shared cardinal rule set. */
  plural: string;
  /** The decimal separator ("." or ","). */
  decimal: string;
  /** The thousands separator ("," "." or a narrow no-break space). */
  group: string;
};

export type Args = Record<string, string | number>;

type Node =
  | { kind: "text"; text: string }
  | { kind: "arg"; name: string }
  | { kind: "pound" }
  | { kind: "plural"; name: string; cases: Record<string, Node[]> }
  | { kind: "select"; name: string; cases: Record<string, Node[]> };

const parsed = new Map<string, Node[]>();

/** `message` with `args` in it, written for `locale`. An argument it names but isn't given stays as `{name}`. */
export function formatMessage(message: string, args: Args, locale: Locale): string {
  let nodes = parsed.get(message);
  if (!nodes) {
    nodes = parse(message);
    parsed.set(message, nodes);
  }
  return render(nodes, args, locale, null);
}

/** A number in the language's digits: grouped thousands, up to two decimals, no trailing zeros. */
export function formatNumber(n: number, locale: Locale): string {
  if (!Number.isFinite(n)) return String(n);
  const negative = n < 0;
  const [whole, fraction = ""] = Math.abs(n).toFixed(2).replace(/0+$/, "").replace(/\.$/, "").split(".");
  const grouped = whole!.replace(/\B(?=(\d{3})+(?!\d))/g, locale.group);
  return `${negative ? "-" : ""}${grouped}${fraction ? locale.decimal + fraction : ""}`;
}

/** The plural category of `n` by the language's rule. */
export function pluralCategory(n: number, locale: Locale): PluralCategory {
  return cardinalCategory(n, locale.plural);
}

/** What `message` asks for: its arguments, by the way it uses them. Throws on a malformed message. */
export function argumentsOf(message: string): Map<string, "text" | "plural" | "select"> {
  const out = new Map<string, "text" | "plural" | "select">();
  const walk = (nodes: Node[]) => {
    for (const n of nodes) {
      if (n.kind === "arg") out.set(n.name, out.get(n.name) ?? "text");
      if (n.kind === "plural" || n.kind === "select") {
        out.set(n.name, n.kind);
        Object.values(n.cases).forEach(walk);
      }
    }
  };
  walk(parse(message));
  return out;
}

function render(nodes: Node[], args: Args, locale: Locale, count: number | null): string {
  let out = "";
  for (const n of nodes) {
    switch (n.kind) {
      case "text":
        out += n.text;
        break;
      case "pound":
        out += count === null ? "#" : formatNumber(count, locale);
        break;
      case "arg": {
        const value = args[n.name];
        out += value === undefined ? `{${n.name}}` : typeof value === "number" ? formatNumber(value, locale) : value;
        break;
      }
      case "plural": {
        const value = Number(args[n.name]);
        const exact = n.cases[`=${value}`];
        const chosen = exact ?? n.cases[pluralCategory(value, locale)] ?? n.cases.other!;
        out += render(chosen, args, locale, value);
        break;
      }
      case "select": {
        const value = String(args[n.name] ?? "");
        out += render(n.cases[value] ?? n.cases.other!, args, locale, count);
        break;
      }
    }
  }
  return out;
}

/** Parse a message, or throw saying where it is malformed. */
export function parse(message: string): Node[] {
  let i = 0;
  const fail = (why: string): never => {
    throw new Error(`${why} at ${i} in: ${message}`);
  };
  const skipSpace = () => {
    while (i < message.length && /\s/.test(message[i]!)) i++;
  };
  const word = () => {
    skipSpace();
    const start = i;
    while (i < message.length && /[A-Za-z0-9_=.-]/.test(message[i]!)) i++;
    if (start === i) fail("expected a name");
    return message.slice(start, i);
  };
  const nodes = (inPlural: boolean, nested: boolean): Node[] => {
    const out: Node[] = [];
    let text = "";
    const flush = () => {
      if (text) out.push({ kind: "text", text });
      text = "";
    };
    while (i < message.length) {
      const c = message[i]!;
      if (c === "}") {
        if (!nested) fail("unexpected }");
        break;
      }
      if (c === "#" && inPlural) {
        flush();
        out.push({ kind: "pound" });
        i++;
        continue;
      }
      if (c !== "{") {
        text += c;
        i++;
        continue;
      }
      flush();
      i++;
      const name = word();
      skipSpace();
      if (message[i] === "}") {
        i++;
        out.push({ kind: "arg", name });
        continue;
      }
      if (message[i] !== ",") fail("expected , or }");
      i++;
      const type = word();
      if (type !== "plural" && type !== "select") fail(`unknown argument type ${type}`);
      skipSpace();
      if (message[i] !== ",") fail("expected ,");
      i++;
      const cases: Record<string, Node[]> = {};
      for (;;) {
        skipSpace();
        if (message[i] === "}") {
          i++;
          break;
        }
        const key = word();
        skipSpace();
        if (message[i] !== "{") fail("expected { after a case");
        i++;
        cases[key] = nodes(type === "plural", true);
        if (message[i] !== "}") fail("unclosed case");
        i++;
      }
      if (!cases.other) fail(`${type} needs an other case`);
      out.push({ kind: type === "plural" ? "plural" : "select", name, cases });
    }
    flush();
    return out;
  };
  const result = nodes(false, false);
  if (i < message.length) fail("unexpected }");
  return result;
}
