import { describe, expect, test } from 'bun:test';
import { hasVisibleSummaryContent, parseSummaryContent } from '../../src/lib/summary-content';

describe('summary content validation', () => {
  test('accepts visible markdown and rejects whitespace or reasoning markers', () => {
    expect(hasVisibleSummaryContent({ markdown: '# Summary\nVisible' })).toBe(true);
    expect(hasVisibleSummaryContent({ markdown: '   ' })).toBe(false);
    expect(hasVisibleSummaryContent({ markdown: '<think>private</think>' })).toBe(false);
    expect(hasVisibleSummaryContent({
      markdown: '<thinking class="internal">private</thinking>',
    })).toBe(false);
    expect(hasVisibleSummaryContent({ markdown: '<thinker>Visible</thinker>' })).toBe(true);
  });

  test('validates BlockNote and legacy summary content', () => {
    expect(hasVisibleSummaryContent({ summary_json: [] })).toBe(false);
    expect(hasVisibleSummaryContent({
      summary_json: [{ content: [{ type: 'text', text: 'Visible' }] }],
    })).toBe(true);
    expect(hasVisibleSummaryContent({
      Decisions: { blocks: [{ content: '   ' }] },
    })).toBe(false);
    expect(hasVisibleSummaryContent({
      Decisions: { blocks: [{ content: 'Ship it' }] },
    })).toBe(true);
  });
});

describe('summary content parsing', () => {
  test.each([
    { markdown: 'Visible' },
    { summary_json: [{ content: [{ type: 'text', text: 'Visible' }] }] },
    { Decisions: { blocks: [{ content: 'Ship it' }] } },
  ])('accepts a summary object and one JSON encoding of it: %j', (summary) => {
    expect(parseSummaryContent(summary)).toEqual(summary);
    expect(parseSummaryContent(JSON.stringify(summary))).toEqual(summary);
  });

  test('rejects malformed JSON', () => {
    expect(parseSummaryContent('{not json')).toBeNull();
  });

  test('rejects an actual double-encoded summary instead of treating a string as summary content', () => {
    const doubleEncoded = JSON.stringify(JSON.stringify({ markdown: 'Visible' }));

    expect(typeof JSON.parse(doubleEncoded)).toBe('string');
    expect(parseSummaryContent(doubleEncoded)).toBeNull();
  });

  test.each([null, undefined, '', 'null', '42', '[]', '{}'])('rejects input without a summary object: %j', (input) => {
    expect(parseSummaryContent(input)).toBeNull();
  });

  test.each([{ markdown: '   ' }, { markdown: '<think>private</think>' }])(
    'rejects empty or reasoning-only content after JSON decoding: %j', (summary) => {
      expect(parseSummaryContent(JSON.stringify(summary))).toBeNull();
    },
  );
});
