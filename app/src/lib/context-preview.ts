/** One native-context preview budget shared by recorded page and current task statements and clients. */
export const CONTEXT_PREVIEW_CHARACTERS = 512;

export function contextPreview(text: string): { text: string; truncated: boolean } {
  let preview = "";
  let characters = 0;
  for (const character of text) {
    if (characters === CONTEXT_PREVIEW_CHARACTERS) return { text: `${preview}…`, truncated: true };
    preview += character;
    characters++;
  }
  return { text: preview, truncated: false };
}
