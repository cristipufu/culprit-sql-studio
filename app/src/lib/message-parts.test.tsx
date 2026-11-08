import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { slot, tx, withElements } from "./message-parts";

test("a message's element arguments take their slots, and its text stays text", () => {
  const message = `Saved for ${slot(0)}; running on ${slot(1)}. <b>`;
  const html = renderToStaticMarkup(<p>{withElements(message, [<b key="a">one</b>, <code key="b">two</code>])}</p>);
  expect(html).toBe("<p>Saved for <b>one</b>; running on <code>two</code>. &lt;b&gt;</p>");
});

test("tx words a catalog message and puts its element arguments in their places", () => {
  const html = renderToStaticMarkup(<p>{tx("ui.drift_panel.compare", {}, { connection: <b key="c">prod</b> })}</p>);
  expect(html).toContain("<b>prod</b>");
  expect(html).not.toContain("{connection}");
});
