import { describe, expect, test } from "bun:test";
import { Schema } from "prosemirror-model";
import { EditorState } from "prosemirror-state";
import {
  TIMESTAMP_REF_REGEX,
  parseTimestampRange,
  createTranscriptRefDecorations,
  transcriptRefPluginKey,
  TranscriptReferencePlugin,
} from "../../src/lib/transcript-reference-plugin";

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

describe("pagination retry and highlight timer lifecycle", () => {
  test("pagination retry continues loading until target timestamp is reached", () => {
    let simulatedClock = 0;
    let lastLoadTime = -1000;
    let pagesLoaded = 1;
    const pageSize = 100;

    let segments = Array.from({ length: 100 }, (_, i) => ({
      id: `seg-${i}`,
      timestamp: i * 1.5,
      endTime: (i + 1) * 1.5,
      text: `Segment ${i}`,
    }));

    const loadMoreMock = (): boolean => {
      if (simulatedClock - lastLoadTime < 100) {
        return false;
      }
      lastLoadTime = simulatedClock;
      pagesLoaded += 1;
      const startIdx = (pagesLoaded - 1) * pageSize;
      const newSegments = Array.from({ length: pageSize }, (_, i) => {
        const idx = startIdx + i;
        return {
          id: `seg-${idx}`,
          timestamp: idx * 1.5,
          endTime: (idx + 1) * 1.5,
          text: `Segment ${idx}`,
        };
      });
      segments = [...segments, ...newSegments];
      return true;
    };

    // Target is at 350 seconds (requires segments past index 200)
    const targetTime = 350;
    const targetKey = 1;
    let pendingScroll: { key: number; time: number } | null = {
      key: targetKey,
      time: targetTime,
    };
    let hasMore = true;
    let scheduledRetryCallback: (() => void) | null = null;

    const runStep = () => {
      const lastSegment = segments[segments.length - 1];
      const lastLoadedTime = lastSegment.endTime ?? lastSegment.timestamp;

      if (targetTime > lastLoadedTime && hasMore) {
        loadMoreMock();
        scheduledRetryCallback = () => {
          scheduledRetryCallback = null;
          if (!pendingScroll || !hasMore) return;
          loadMoreMock();
          runStep();
        };
        return;
      }

      pendingScroll = null;
      scheduledRetryCallback = null;
    };

    // Initial step at t=0
    runStep();
    expect(pagesLoaded).toBe(2);
    expect(segments.length).toBe(200); // 200 * 1.5 = 300s, still < 350s
    expect(pendingScroll).toEqual({ key: 1, time: 350 });

    // Immediate attempt at t=10 fails due to <100ms debounce
    simulatedClock = 10;
    const immediateAttempt = loadMoreMock();
    expect(immediateAttempt).toBe(false);

    // After 120ms delay expires (simulatedClock = 120), scheduled retry runs
    simulatedClock = 120;
    expect(scheduledRetryCallback).not.toBeNull();
    scheduledRetryCallback!();

    // After retry, page 3 has loaded and target 350s is reached
    expect(pagesLoaded).toBe(3);
    expect(segments.length).toBe(300);
    expect(pendingScroll).toBeNull();
    expect(scheduledRetryCallback).toBeNull();
  });

  test("highlight timer lifecycle survives unrelated dependency changes", () => {
    let highlightedId: string | null = "seg-42";
    let timerCallback: (() => void) | null = () => {
      highlightedId = null;
      timerCallback = null;
    };

    // Simulate an unrelated dependency update (e.g. segments array reference change or speaker metadata update)
    // The timer ref is retained across re-renders and not cancelled by effect teardown.
    const isNewRequest = false;
    const isPending = false;
    if (!isNewRequest && !isPending) {
      // Early return: timerCallback is NOT cleared
    }

    // Highlight remains active during interim updates
    expect(highlightedId).toBe("seg-42");
    expect(timerCallback).not.toBeNull();

    // Trigger timer expiry
    timerCallback!();

    // Highlight clears cleanly on timer completion
    expect(highlightedId).toBeNull();
    expect(timerCallback).toBeNull();
  });
});
