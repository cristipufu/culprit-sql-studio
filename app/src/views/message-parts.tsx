import { Fragment, type ReactNode } from "react";

// A message whose arguments are elements (a command in <code>, a name in bold): t() words the whole sentence with a
// slot for each element, and `withElements` puts the elements in their slots. The catalog's text stays text.

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
