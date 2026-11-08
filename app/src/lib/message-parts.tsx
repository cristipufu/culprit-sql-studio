import { Fragment, type ReactNode } from "react";
import { t } from "./i18n";
import type { Args } from "./message-format";

// A message whose arguments are elements (a command in <code>, a name in bold): t() words the whole sentence with a
// slot for each element, and `withElements` puts the elements in their slots. The catalog's text stays text.
// `tx` does both: `tx("ui.x.y", { count }, { table: <b>{name}</b> })`.

const MARK = "\u0001";
const SLOTS = new RegExp(`${MARK}(\\d+)${MARK}`);

/** The argument standing for `elements[i]` of `withElements`. */
export function slot(i: number): string {
  return `${MARK}${i}${MARK}`;
}

/** `message` (said by t() with `slot(i)` arguments) with each slot replaced by its element. */
export function withElements(message: string, elements: ReactNode[]): ReactNode {
  const parts = message.split(SLOTS);
  return (
    <>
      {parts.map((part, i) => (i % 2 === 0 ? part : <Fragment key={i}>{elements[Number(part)]}</Fragment>))}
    </>
  );
}

/** The message `key` with `args`, and each of `elements` (a `{name}` of the message) in its place. */
export function tx(key: string, args: Args, elements: Record<string, ReactNode>): ReactNode {
  const names = Object.keys(elements);
  const slots = Object.fromEntries(names.map((name, i) => [name, slot(i)]));
  return withElements(t(key, { ...args, ...slots }), names.map((name) => elements[name]));
}
