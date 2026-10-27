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

/**
 * Whether the cited time may refer to a segment that is not loaded yet. Pages are ordered by start
 * time, so every unloaded segment starts at or after the last loaded start. Reference times are
 * floored to whole seconds, so the cited segment can start up to a second after the cited time.
 * Comparing against the last loaded start (not its end) also covers overlapping segments at a page edge.
 */
export function targetMayBeUnloaded(segments: NavigableSegment[], time: number): boolean {
  const last = segments[segments.length - 1];
  if (!last) return false;
  return time + 1 > last.timestamp;
}
