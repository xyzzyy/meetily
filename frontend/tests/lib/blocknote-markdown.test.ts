import { afterEach, describe, expect, mock, spyOn, test } from "bun:test";
import type { Block } from "@blocknote/core";
import { blocksToMarkdownSafely } from "../../src/lib/blocknote-markdown";

describe("blocksToMarkdownSafely", () => {
  afterEach(() => {
    mock.restore();
  });

  test("passes the supplied blocks to the converter and returns its markdown", async () => {
    const blocks: Block[] = [{
      id: "block-1",
      type: "paragraph",
      props: { textColor: "default", backgroundColor: "default", textAlignment: "left" },
      content: [{ type: "text", text: "Meeting notes", styles: {} }],
      children: [],
    }];
    const editor = {
      blocksToMarkdownLossy: mock(async (_blocks: Block[]) => "# Summary"),
    };

    const result = await blocksToMarkdownSafely(editor, blocks, {
      source: "test-success",
    });

    expect(result).toEqual({
      markdown: "# Summary",
      ok: true,
    });
    expect(editor.blocksToMarkdownLossy).toHaveBeenCalledTimes(1);
    expect(editor.blocksToMarkdownLossy).toHaveBeenCalledWith(blocks);
  });

  test("returns fallback markdown when conversion throws", async () => {
    const error = new Error("conversion failed");
    const editor = {
      blocksToMarkdownLossy: mock(async () => {
        throw error;
      }),
    };
    const consoleError = spyOn(console, "error").mockImplementation(() => {});

    const result = await blocksToMarkdownSafely(editor, [{ id: "block-1" }] as any, {
      source: "test-fallback",
      fallbackMarkdown: "existing markdown",
    });

    expect(result).toEqual({
      markdown: "existing markdown",
      ok: false,
    });
    expect(consoleError).toHaveBeenCalledTimes(1);
    expect(consoleError).toHaveBeenCalledWith(
      "Failed to convert BlockNote blocks to markdown",
      {
        source: "test-fallback",
        blocksCount: 1,
        error,
      },
    );
  });

  test("omits markdown when conversion throws without fallback", async () => {
    const editor = {
      blocksToMarkdownLossy: mock(async () => {
        throw new Error("conversion failed");
      }),
    };
    spyOn(console, "error").mockImplementation(() => {});

    const result = await blocksToMarkdownSafely(editor, [] as any, {
      source: "test-empty-fallback",
    });

    expect(result).toEqual({
      markdown: undefined,
      ok: false,
    });
  });
});
