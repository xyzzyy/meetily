export interface NavigableSegment {
  timestamp: number; // audio_start_time in seconds
  endTime?: number; // audio_end_time in seconds
}

/**
 * Picks the segment a transcript reference points at.
 *
 * References are whole seconds (floored when the summary prompt is built), so a segment starting
 * in the same second wins; otherwise a fractional start (872.7s cited as 872) would select the
 * preceding segment. Falls back to a segment whose range contains the time, then to the nearest one.
 */
export function findTargetSegmentIndex(segments: NavigableSegment[], time: number): number {
  const targetSecond = Math.floor(time);
  let index = segments.findIndex(s => Math.floor(s.timestamp) === targetSecond);
  if (index !== -1) return index;

  index = segments.findIndex(s => s.timestamp <= time && (s.endTime ?? s.timestamp) >= time);
  if (index !== -1) return index;

  let best = 0;
  let bestDist = Infinity;
  for (let i = 0; i < segments.length; i++) {
    const s = segments[i];
    const sEnd = s.endTime ?? s.timestamp;
    const dist = time < s.timestamp ? s.timestamp - time : time > sEnd ? time - sEnd : 0;
    if (dist < bestDist) {
      bestDist = dist;
      best = i;
    }
  }
  return best;
}

export interface LoadMoreState {
  hasMore: boolean;
  isLoadingMore: boolean;
  onLoadMore?: () => void;
}

export interface LoadMoreRetryOptions {
  /** Returns the latest pagination state, so retries never act on a stale closure */
  getState: () => LoadMoreState;
  /** Called when retries are exhausted while more pages are still expected */
  onGiveUp: () => void;
  maxRetries: number;
  delayMs: number;
}

/**
 * Keeps requesting pages for a pending jump. `usePaginatedTranscripts` debounces calls less than
 * 100ms apart, so a single call can be dropped; retry until cancelled, out of pages, or out of retries.
 * Returns a cancel function.
 */
export function startLoadMoreRetry({ getState, onGiveUp, maxRetries, delayMs }: LoadMoreRetryOptions): () => void {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let attempts = 0;

  const schedule = () => {
    timer = setTimeout(() => {
      timer = null;
      const state = getState();
      if (!state.hasMore || !state.onLoadMore) return;
      if (++attempts > maxRetries) {
        onGiveUp();
        return;
      }
      if (!state.isLoadingMore) state.onLoadMore();
      schedule();
    }, delayMs);
  };
  schedule();

  return () => {
    if (timer) clearTimeout(timer);
    timer = null;
  };
}

/**
 * Highlights one segment at a time and clears it after `durationMs`. Showing a new segment restarts
 * the timer; unrelated re-renders never touch it.
 */
export function createHighlightTimer(setHighlight: (id: string | null) => void, durationMs: number) {
  let timer: ReturnType<typeof setTimeout> | null = null;

  const cancel = () => {
    if (timer) clearTimeout(timer);
    timer = null;
  };

  return {
    show(id: string) {
      cancel();
      setHighlight(id);
      timer = setTimeout(() => {
        timer = null;
        setHighlight(null);
      }, durationMs);
    },
    clear() {
      cancel();
      setHighlight(null);
    },
    /** Cancels the timer without touching the highlight (unmount) */
    dispose: cancel,
  };
}
