import { Extension } from '@tiptap/react';
import { Plugin, PluginKey } from '@tiptap/pm/state';
import { Decoration, DecorationSet } from '@tiptap/pm/view';
import type { Node as ProseMirrorNode } from '@tiptap/pm/model';

/**
 * Matches [MM:SS-MM:SS], [MM:SS–MM:SS], [MM:SS—MM:SS], or single [MM:SS],
 * supporting 1 to 4 digit minutes and optional whitespace around delimiters.
 */
export const TIMESTAMP_REF_REGEX = /\[(\d{1,4}:\d{2})(?:\s*[-–—]\s*(\d{1,4}:\d{2}))?\]/g;

export interface ParsedTimestampRange {
  startTime: number;
  endTime: number;
}

export function parseTimestampRange(match: RegExpExecArray): ParsedTimestampRange {
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
  return { startTime, endTime };
}

export function createTranscriptRefDecorations(doc: ProseMirrorNode): DecorationSet {
  const decorations: Decoration[] = [];

  doc.descendants((node, pos) => {
    if (node.isText && node.text) {
      TIMESTAMP_REF_REGEX.lastIndex = 0;
      let match: RegExpExecArray | null;
      while ((match = TIMESTAMP_REF_REGEX.exec(node.text)) !== null) {
        const from = pos + match.index;
        const to = from + match[0].length;
        const { startTime, endTime } = parseTimestampRange(match);

        decorations.push(
          Decoration.inline(from, to, {
            class: 'transcript-ref inline text-xs font-semibold text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-950/40 border border-blue-200 dark:border-blue-800 rounded px-1 mx-0.5 cursor-pointer select-none hover:bg-blue-100 dark:hover:bg-blue-900/60 hover:text-blue-800 dark:hover:text-blue-300 transition-colors',
            'data-start': String(startTime),
            'data-end': String(endTime),
            role: 'button',
            tabindex: '0',
            title: `Jump to transcript ${match[0]}`,
            'aria-label': `Jump to transcript at ${match[0]}`,
          })
        );
      }
    }
  });

  return DecorationSet.create(doc, decorations);
}

export const transcriptRefPluginKey = new PluginKey<DecorationSet>('transcriptRefs');

export const TranscriptReferencePlugin = new Plugin<DecorationSet>({
  key: transcriptRefPluginKey,
  state: {
    init(_, { doc }) {
      return createTranscriptRefDecorations(doc);
    },
    apply(tr, oldSet) {
      return tr.docChanged ? createTranscriptRefDecorations(tr.doc) : oldSet.map(tr.mapping, tr.doc);
    },
  },
  props: {
    decorations(state) {
      return transcriptRefPluginKey.getState(state);
    },
  },
});

export const TranscriptReferenceExtension = Extension.create({
  name: 'transcriptReference',
  addProseMirrorPlugins() {
    return [TranscriptReferencePlugin];
  },
});
