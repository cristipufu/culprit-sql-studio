import { Fragment, type ReactNode } from "react";

/** Plain text with `inline code` spans, as the checks write it. Never renders HTML from the database. */
export function Prose({ text }: { text: string }) {
  const parts = text.split("`");
  return (
    <>
      {parts.map((part, i) =>
        i % 2 === 1 ? (
          <code key={i} className="inline-code">
            {part}
          </code>
        ) : (
          part
        ),
      )}
    </>
  );
}

const MARK = "\u0000";

/** Where an element goes in a message: pass `slot("name")` as that argument, then render the message with `Rich`. */
export function slot(name: string): string {
  return `${MARK}${name}${MARK}`;
}

/**
 * A catalog message with elements in it (a bold number, a code span), kept one whole sentence for translators: format it
 * with `slot(name)` as each such argument, and `parts[name]` renders where the language put it. Every other piece stays
 * text, never HTML.
 */
export function Rich({ text, parts }: { text: string; parts: Record<string, ReactNode> }) {
  return <>{text.split(MARK).map((piece, i) => (i % 2 === 1 ? <Fragment key={i}>{parts[piece]}</Fragment> : piece))}</>;
}
