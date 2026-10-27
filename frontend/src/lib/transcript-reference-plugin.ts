import { Extension } from '@tiptap/react';
import { Plugin, PluginKey, type Transaction } from '@tiptap/pm/state';
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

const REF_CLASS =
  'transcript-ref inline text-xs font-semibold text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-950/40 border border-blue-200 dark:border-blue-800 rounded px-1 mx-0.5 cursor-pointer hover:bg-blue-100 dark:hover:bg-blue-900/60 hover:text-blue-800 dark:hover:text-blue-300 transition-colors';

/** Decorations for every reference inside `node`, whose content starts at absolute position `contentStart`. */
function decorationsWithin(node: ProseMirrorNode, contentStart: number): Decoration[] {
  const decorations: Decoration[] = [];

  node.descendants((child, pos) => {
    if (!child.isText || !child.text) return;
    TIMESTAMP_REF_REGEX.lastIndex = 0;
    let match: RegExpExecArray | null;
    while ((match = TIMESTAMP_REF_REGEX.exec(child.text)) !== null) {
      const from = contentStart + pos + match.index;
      const to = from + match[0].length;
      const { startTime, endTime } = parseTimestampRange(match);

      decorations.push(
        Decoration.inline(from, to, {
          class: REF_CLASS,
          'data-start': String(startTime),
          'data-end': String(endTime),
          title: `Jump to transcript ${match[0]}`,
        })
      );
    }
  });

  return decorations;
}

export function createTranscriptRefDecorations(doc: ProseMirrorNode): DecorationSet {
  return DecorationSet.create(doc, decorationsWithin(doc, 0));
}

/**
 * Maps the previous decorations through `tr` and rescans only the textblocks the transaction
 * changed, so typing in a long summary does not rescan the whole document on every keystroke.
 */
export function updateTranscriptRefDecorations(tr: Transaction, oldSet: DecorationSet): DecorationSet {
  if (!tr.docChanged) return oldSet;

  const { doc, mapping } = tr;
  const size = doc.content.size;
  let set = oldSet.map(mapping, doc);

  mapping.maps.forEach((stepMap, i) => {
    // Changed ranges are reported in this step's output; map them to the final document
    const rest = mapping.slice(i + 1);
    stepMap.forEach((_oldStart, _oldEnd, newStart, newEnd) => {
      const from = Math.min(Math.max(rest.map(newStart, -1), 0), size);
      const to = Math.min(Math.max(rest.map(newEnd, 1), from), size);
      doc.nodesBetween(from, to, (node, pos) => {
        if (!node.isTextblock) return true;
        set = set.remove(set.find(pos, pos + node.nodeSize));
        set = set.add(doc, decorationsWithin(node, pos + 1));
        return false;
      });
    });
  });

  return set;
}

export const transcriptRefPluginKey = new PluginKey<DecorationSet>('transcriptRefs');

export const TranscriptReferencePlugin = new Plugin<DecorationSet>({
  key: transcriptRefPluginKey,
  state: {
    init(_, { doc }) {
      return createTranscriptRefDecorations(doc);
    },
    apply(tr, oldSet) {
      return updateTranscriptRefDecorations(tr, oldSet);
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
