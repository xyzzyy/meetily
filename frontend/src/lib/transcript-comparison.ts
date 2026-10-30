export interface TranscriptChange { text: string; changed: boolean }

/** Preserve both texts exactly. Bound comparison work for long meetings. */
export function compareTranscriptText(before: string, after: string): {
  before: TranscriptChange[]; after: TranscriptChange[];
} {
  const a = before.match(/\S+\s*|\s+/g) || [];
  const b = after.match(/\S+\s*|\s+/g) || [];
  const left: TranscriptChange[] = [];
  const right: TranscriptChange[] = [];
  const same = (i: number, j: number) => a[i].trim() === b[j].trim();
  let start = 0;
  while (start < a.length && start < b.length && same(start, start)) {
    left.push({ text: a[start], changed: false });
    right.push({ text: b[start], changed: false });
    start++;
  }
  let endA = a.length, endB = b.length;
  while (endA > start && endB > start && same(endA - 1, endB - 1)) { endA--; endB--; }
  const height = endA - start + 1, width = endB - start + 1;
  if (height * width > 2_000_000) {
    left.push({ text: a.slice(start, endA).join(''), changed: true });
    right.push({ text: b.slice(start, endB).join(''), changed: true });
  } else {
    const lengths = new Uint16Array(height * width);
    for (let i = height - 2; i >= 0; i--) for (let j = width - 2; j >= 0; j--) {
      lengths[i * width + j] = same(start + i, start + j)
        ? 1 + lengths[(i + 1) * width + j + 1]
        : Math.max(lengths[(i + 1) * width + j], lengths[i * width + j + 1]);
    }
    let i = 0, j = 0;
    while (i < height - 1 || j < width - 1) {
      if (i < height - 1 && j < width - 1 && same(start + i, start + j)) {
        left.push({ text: a[start + i++], changed: false });
        right.push({ text: b[start + j++], changed: false });
      } else if (i < height - 1 && (j === width - 1 || lengths[(i + 1) * width + j] >= lengths[i * width + j + 1])) {
        left.push({ text: a[start + i++], changed: true });
      } else right.push({ text: b[start + j++], changed: true });
    }
  }
  a.slice(endA).forEach(text => left.push({ text, changed: false }));
  b.slice(endB).forEach(text => right.push({ text, changed: false }));
  return { before: left, after: right };
}
