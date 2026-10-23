import { describe, expect, test } from "bun:test";

describe("transcript reference regex and time parsing", () => {
  const TIMESTAMP_REF = /\[(\d{1,4}:\d{2})(?:\s*[-–—]\s*(\d{1,4}:\d{2}))?\]/g;

  function parseReferences(text: string) {
    const results: Array<{ text: string; start: number; end: number }> = [];
    let match: RegExpExecArray | null;
    TIMESTAMP_REF.lastIndex = 0;

    while ((match = TIMESTAMP_REF.exec(text)) !== null) {
      const startParts = match[1].split(':');
      const startM = parseInt(startParts[0], 10);
      const startS = parseInt(startParts[1], 10);
      const startTime = startM * 60 + startS;

      let endTime = startTime;
      if (match[2]) {
        const endParts = match[2].split(':');
        const endM = parseInt(endParts[0], 10);
        const endS = parseInt(endParts[1], 10);
        endTime = endM * 60 + endS;
      }

      results.push({ text: match[0], start: startTime, end: endTime });
    }
    return results;
  }

  test("parses standard hyphen reference", () => {
    const refs = parseReferences("Action item [12:34-13:45]");
    expect(refs).toEqual([{ text: "[12:34-13:45]", start: 754, end: 825 }]);
  });

  test("parses en-dash and em-dash references", () => {
    const refs = parseReferences("Item 1 [01:10–02:20] and Item 2 [05:00—06:00]");
    expect(refs).toEqual([
      { text: "[01:10–02:20]", start: 70, end: 140 },
      { text: "[05:00—06:00]", start: 300, end: 360 },
    ]);
  });

  test("parses spaces around delimiters", () => {
    const refs = parseReferences("Item 1 [14:32 - 15:18] and Item 2 [20:00  –  21:30]");
    expect(refs).toEqual([
      { text: "[14:32 - 15:18]", start: 872, end: 918 },
      { text: "[20:00  –  21:30]", start: 1200, end: 1290 },
    ]);
  });

  test("parses point-in-time reference", () => {
    const refs = parseReferences("Key moment: [05:30]");
    expect(refs).toEqual([{ text: "[05:30]", start: 330, end: 330 }]);
  });

  test("parses multi-digit minutes for long meetings", () => {
    const refs = parseReferences("Wrap up [125:10-126:40]");
    expect(refs).toEqual([{ text: "[125:10-126:40]", start: 7510, end: 7600 }]);
  });
});
