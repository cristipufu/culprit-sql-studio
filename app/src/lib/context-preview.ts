/** Bound native text by Unicode code points; callers share a default display budget. */
export const CONTEXT_PREVIEW_CHARACTERS = 512;

export function contextPreview(text: string, maximumCharacters = CONTEXT_PREVIEW_CHARACTERS): { text: string; truncated: boolean } {
  let preview = "";
  let characters = 0;
  for (const character of text) {
    if (characters === maximumCharacters) return { text: `${preview}…`, truncated: true };
    preview += character;
    characters++;
  }
  return { text: preview, truncated: false };
}
