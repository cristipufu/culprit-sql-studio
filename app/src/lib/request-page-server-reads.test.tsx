import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { RequestPageServerReads } from "../components/RequestPageServerReads";
import { requestPageServerReadReport } from "../test/query-progress";

test("request count presentation uses exact Rust metric text with folded scope and no native-number coercion", () => {
  const html = renderToStaticMarkup(<RequestPageServerReads report={requestPageServerReadReport()} />);
  expect(html).toContain("9,007,199,254,740,993");
  expect(html).not.toContain("9,007,199,254,740,992");
  expect(html).not.toContain("open=\"\"");
  expect(html.match(/<details/g)).toHaveLength(3);
  expect(html).toContain("does not identify the statement");
  expect(html).toContain("does not measure physical storage I/O");
});

test("shared report messages remain text even when a caller supplies markup", () => {
  const report = requestPageServerReadReport(); report.summary = "<script>private-marker</script>";
  const html = renderToStaticMarkup(<RequestPageServerReads report={report} />);
  expect(html).not.toContain("<script>"); expect(html).toContain("&lt;script&gt;");
});
