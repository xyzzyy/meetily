import { describe, expect, test } from "bun:test";
import { Schema } from "@tiptap/pm/model";
import { EditorState } from "@tiptap/pm/state";
import {
  TIMESTAMP_REF_REGEX,
  parseTimestampRange,
  createTranscriptRefDecorations,
  transcriptRefPluginKey,
  TranscriptReferencePlugin,
} from "../../src/lib/transcript-reference-plugin";
import { findTargetSegmentIndex, targetMayBeUnloaded } from "../../src/lib/transcript-navigation";

interface InlineDecorationAttrs {
  class?: string;
  "data-start"?: string;
  "data-end"?: string;
  role?: string;
  tabindex?: string;
  title?: string;
  "aria-label"?: string;
}

interface InlineDecorationInstance {
  from: number;
  to: number;
  type: {
    attrs: InlineDecorationAttrs;
  };
}

describe("transcript reference regex and time parsing", () => {
  function parseReferences(text: string) {
    const results: Array<{ text: string; start: number; end: number }> = [];
    let match: RegExpExecArray | null;
    TIMESTAMP_REF_REGEX.lastIndex = 0;

    while ((match = TIMESTAMP_REF_REGEX.exec(text)) !== null) {
      const { startTime, endTime } = parseTimestampRange(match);
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

describe("ProseMirror transcript reference decorations", () => {
  const schema = new Schema({
    nodes: {
      doc: { content: "block+" },
      paragraph: { group: "block", content: "text*" },
      text: { inline: true },
    },
  });

  test("generates inline decorations without mutating the document model", () => {
    const doc = schema.node("doc", null, [
      schema.node("paragraph", null, [
        schema.text("Decision: Migrate auth. [14:32-15:18] Follow-up."),
      ]),
    ]);

    const decorations = createTranscriptRefDecorations(doc);
    const found = decorations.find() as unknown as InlineDecorationInstance[];

    expect(found.length).toBe(1);
    expect(found[0].from).toBe(25);
    expect(found[0].to).toBe(38);

    const attrs = found[0].type.attrs;
    expect(attrs.class).toContain("transcript-ref");
    expect(attrs["data-start"]).toBe("872");
    expect(attrs["data-end"]).toBe("918");
    expect(attrs.title).toBe("Jump to transcript [14:32-15:18]");
    // Inside contenteditable the span cannot take focus, so it must not claim to be a keyboard button,
    // and its text must stay selectable for editing
    expect(attrs.role).toBeUndefined();
    expect(attrs.tabindex).toBeUndefined();
    expect(attrs.class).not.toContain("select-none");

    // Verify document text is unmodified
    expect(doc.textBetween(0, doc.content.size)).toBe(
      "Decision: Migrate auth. [14:32-15:18] Follow-up."
    );
  });

  test("preserves decoration through transactions and typing edits", () => {
    const doc = schema.node("doc", null, [
      schema.node("paragraph", null, [
        schema.text("Item [10:00-11:00]."),
      ]),
    ]);

    let state = EditorState.create({
      schema,
      plugins: [TranscriptReferencePlugin],
      doc,
    });

    let decos = (transcriptRefPluginKey.getState(state)?.find() || []) as unknown as InlineDecorationInstance[];
    expect(decos[0].from).toBe(6);
    expect(decos[0].to).toBe(19);

    // Insert text before reference: decoration shifts without corruption
    const tr = state.tr.insertText("Important ", 1);
    state = state.apply(tr);

    decos = (transcriptRefPluginKey.getState(state)?.find() || []) as unknown as InlineDecorationInstance[];
    expect(decos[0].from).toBe(16);
    expect(decos[0].to).toBe(29);
    expect(decos[0].type.attrs["data-start"]).toBe("600");
    expect(decos[0].type.attrs["data-end"]).toBe("660");
  });

  test("decorates multiple references across multiple blocks", () => {
    const doc = schema.node("doc", null, [
      schema.node("paragraph", null, [
        schema.text("First point [01:00-02:00] and second [03:00–04:00]."),
      ]),
      schema.node("paragraph", null, [
        schema.text("Third point [05:00]."),
      ]),
    ]);

    const decorations = createTranscriptRefDecorations(doc);
    const found = decorations.find() as unknown as InlineDecorationInstance[];
    expect(found.length).toBe(3);

    expect(found[0].type.attrs["data-start"]).toBe("60");
    expect(found[0].type.attrs["data-end"]).toBe("120");

    expect(found[1].type.attrs["data-start"]).toBe("180");
    expect(found[1].type.attrs["data-end"]).toBe("240");

    expect(found[2].type.attrs["data-start"]).toBe("300");
    expect(found[2].type.attrs["data-end"]).toBe("300");
  });

  describe("incremental updates", () => {
    const summarize = (set: { find: () => unknown[] } | undefined) =>
      ((set?.find() ?? []) as InlineDecorationInstance[])
        .map(d => `${d.from}-${d.to}:${d.type.attrs["data-start"]}-${d.type.attrs["data-end"]}`)
        .sort();

    const stateFor = (...paragraphs: string[]) =>
      EditorState.create({
        schema,
        plugins: [TranscriptReferencePlugin],
        doc: schema.node("doc", null, paragraphs.map(text => schema.node("paragraph", null, text ? [schema.text(text)] : []))),
      });

    // The incremental result must always equal a full rescan of the new document
    const expectMatchesFullScan = (state: EditorState) => {
      expect(summarize(transcriptRefPluginKey.getState(state))).toEqual(
        summarize(createTranscriptRefDecorations(state.doc))
      );
    };

    test("typing inside a reference removes its decoration", () => {
      let state = stateFor("Item [10:00-11:00].");
      state = state.apply(state.tr.insertText("x", 7)); // "[1x0:00-11:00]"
      expect(summarize(transcriptRefPluginKey.getState(state))).toEqual([]);
      expectMatchesFullScan(state);
    });

    test("finishing a reference by typing adds its decoration", () => {
      let state = stateFor("Item [10:0");
      state = state.apply(state.tr.insertText("0]", state.doc.content.size - 1));
      expect(summarize(transcriptRefPluginKey.getState(state))).toEqual(["6-13:600-600"]);
      expectMatchesFullScan(state);
    });

    test("splitting and rejoining a paragraph through a reference", () => {
      let state = stateFor("Item [10:00] end.");
      state = state.apply(state.tr.split(9)); // "Item [10:" | "00] end."
      expect(summarize(transcriptRefPluginKey.getState(state))).toEqual([]);
      expectMatchesFullScan(state);

      state = state.apply(state.tr.join(10)); // join the two paragraphs back
      expect(summarize(transcriptRefPluginKey.getState(state))).toEqual(["6-13:600-600"]);
      expectMatchesFullScan(state);
    });

    test("multi-step transactions across blocks match a full rescan", () => {
      let state = stateFor("A [01:00].", "B [02:00-03:00].", "C plain.");
      const tr = state.tr;
      tr.insertText(" [04:00]", tr.doc.content.size - 1); // add a reference to the last block
      tr.delete(15, 16); // remove the "[" of "[02:00-03:00]" in the second block
      state = state.apply(tr);
      expectMatchesFullScan(state);
      expect(summarize(transcriptRefPluginKey.getState(state)).length).toBe(2);
    });

    test("leaves decorations in untouched blocks as they were", () => {
      let state = stateFor("A [01:00].", "B plain.");
      const before = (transcriptRefPluginKey.getState(state)?.find() ?? []) as unknown as InlineDecorationInstance[];

      state = state.apply(state.tr.insertText("more ", state.doc.content.size - 1));
      const after = (transcriptRefPluginKey.getState(state)?.find() ?? []) as unknown as InlineDecorationInstance[];

      expect(after.length).toBe(1);
      // Same decoration spec object: the first block was mapped, not rebuilt
      expect(after[0].type).toBe(before[0].type);
      expectMatchesFullScan(state);
    });
  });
});

describe("findTargetSegmentIndex", () => {
  const segments = [
    { timestamp: 0, endTime: 10 },
    { timestamp: 860, endTime: 872.9 },
    { timestamp: 872.7, endTime: 900 },
    { timestamp: 1000, endTime: 1010 },
  ];

  test("prefers the segment starting in the cited second over an overlapping predecessor", () => {
    // [14:32] floors 872.7s to 872; the previous segment also contains 872
    expect(findTargetSegmentIndex(segments, 872)).toBe(2);
  });

  test("falls back to the segment whose range contains the time", () => {
    expect(findTargetSegmentIndex(segments, 880)).toBe(2);
    expect(findTargetSegmentIndex(segments, 5)).toBe(0);
  });

  test("falls back to the nearest segment when no range contains the time", () => {
    expect(findTargetSegmentIndex(segments, 960)).toBe(3);
    expect(findTargetSegmentIndex(segments, 2000)).toBe(3);
    expect(findTargetSegmentIndex(segments, 30)).toBe(0);
  });

  test("treats segments without an end time as points", () => {
    expect(findTargetSegmentIndex([{ timestamp: 10 }, { timestamp: 50 }], 48)).toBe(1);
  });
});

describe("targetMayBeUnloaded", () => {
  const loaded = [{ timestamp: 0, endTime: 100 }, { timestamp: 800, endTime: 871.2 }];

  test("is true when the cited second reaches the last loaded start or later", () => {
    // [14:31] floors 871.2s to 871; the next page's first segment starts at 871.2
    expect(targetMayBeUnloaded(loaded, 871)).toBe(true);
    expect(targetMayBeUnloaded(loaded, 900)).toBe(true);
    expect(targetMayBeUnloaded(loaded, 800)).toBe(true);
  });

  test("covers an overlapping segment at the page edge", () => {
    // Last loaded segment spans 100-130; the next page starts with an overlapping one at 120
    expect(targetMayBeUnloaded([{ timestamp: 100, endTime: 130 }], 120)).toBe(true);
  });

  test("is false when the cited second is before the last loaded start", () => {
    expect(targetMayBeUnloaded(loaded, 5)).toBe(false);
    expect(targetMayBeUnloaded(loaded, 798)).toBe(false);
  });

  test("tolerates segments without an end time and an empty list", () => {
    expect(targetMayBeUnloaded([{ timestamp: 50 }], 50)).toBe(true);
    expect(targetMayBeUnloaded([], 10)).toBe(false);
  });
});
