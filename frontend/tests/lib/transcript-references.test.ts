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
import {
  createHighlightTimer,
  findTargetSegmentIndex,
  startLoadMoreRetry,
  type LoadMoreState,
} from "../../src/lib/transcript-navigation";

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
    expect(attrs.role).toBe("button");
    expect(attrs.tabindex).toBe("0");
    expect(attrs["aria-label"]).toBe("Jump to transcript at [14:32-15:18]");

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
});

const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

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

describe("startLoadMoreRetry", () => {
  const options = { maxRetries: 3, delayMs: 1 };

  test("keeps requesting pages until no more are available", async () => {
    let calls = 0;
    const state: LoadMoreState = { hasMore: true, isLoadingMore: false, onLoadMore: () => { calls++; } };
    startLoadMoreRetry({ ...options, maxRetries: 100, getState: () => state, onGiveUp: () => { throw new Error("gave up"); } });

    await sleep(20);
    expect(calls).toBeGreaterThan(1);

    state.hasMore = false;
    await sleep(10);
    const callsAfterStop = calls;
    await sleep(20);
    expect(calls).toBe(callsAfterStop);
  });

  test("skips the request while a page is already loading", async () => {
    let calls = 0;
    const state: LoadMoreState = { hasMore: true, isLoadingMore: true, onLoadMore: () => { calls++; } };
    const cancel = startLoadMoreRetry({ ...options, maxRetries: 100, getState: () => state, onGiveUp: () => {} });

    await sleep(20);
    cancel();
    expect(calls).toBe(0);
  });

  test("reads the latest state on every attempt", async () => {
    const seen: string[] = [];
    const state: LoadMoreState = { hasMore: true, isLoadingMore: false, onLoadMore: () => { seen.push("old"); } };
    const cancel = startLoadMoreRetry({ ...options, maxRetries: 100, getState: () => state, onGiveUp: () => {} });

    await sleep(10);
    state.onLoadMore = () => { seen.push("new"); };
    await sleep(15);
    cancel();
    expect(seen).toContain("new");
  });

  test("gives up after maxRetries when pages never arrive", async () => {
    let calls = 0;
    let gaveUp = 0;
    const state: LoadMoreState = { hasMore: true, isLoadingMore: false, onLoadMore: () => { calls++; } };
    startLoadMoreRetry({ ...options, getState: () => state, onGiveUp: () => { gaveUp++; } });

    await sleep(50);
    expect(gaveUp).toBe(1);
    expect(calls).toBe(3);
  });

  test("cancel stops further attempts", async () => {
    let calls = 0;
    const state: LoadMoreState = { hasMore: true, isLoadingMore: false, onLoadMore: () => { calls++; } };
    const cancel = startLoadMoreRetry({ ...options, maxRetries: 100, getState: () => state, onGiveUp: () => {} });

    cancel();
    await sleep(20);
    expect(calls).toBe(0);
  });
});

describe("createHighlightTimer", () => {
  test("highlights a segment and clears it after the duration", async () => {
    const history: Array<string | null> = [];
    const timer = createHighlightTimer(id => history.push(id), 5);

    timer.show("seg-42");
    expect(history).toEqual(["seg-42"]);

    await sleep(20);
    expect(history).toEqual(["seg-42", null]);
  });

  test("showing a new segment restarts the timer instead of clearing early", async () => {
    const history: Array<string | null> = [];
    const timer = createHighlightTimer(id => history.push(id), 30);

    timer.show("a");
    await sleep(20);
    timer.show("b");
    await sleep(20); // past a's original expiry, before b's
    expect(history).toEqual(["a", "b"]);

    await sleep(30);
    expect(history).toEqual(["a", "b", null]);
  });

  test("clear removes the highlight and cancels the pending expiry", async () => {
    const history: Array<string | null> = [];
    const timer = createHighlightTimer(id => history.push(id), 5);

    timer.show("a");
    timer.clear();
    await sleep(20);
    expect(history).toEqual(["a", null]);
  });

  test("dispose cancels the timer without updating state", async () => {
    const history: Array<string | null> = [];
    const timer = createHighlightTimer(id => history.push(id), 5);

    timer.show("a");
    timer.dispose();
    await sleep(20);
    expect(history).toEqual(["a"]);
  });
});
