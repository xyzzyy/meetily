import { describe, expect, test } from 'bun:test';
import { getTranscriptRevealOffset } from '../../src/lib/transcript-navigation';

describe('revealing transcript references', () => {
  const viewport = { scrollTop: 500, viewportTop: 100, viewportHeight: 300 };

  test('keeps a fully visible row at its current position', () => {
    expect(getTranscriptRevealOffset({ ...viewport, targetTop: 160, targetHeight: 80 })).toBe(500);
  });

  test('keeps a row at either viewport edge in place', () => {
    expect(getTranscriptRevealOffset({ ...viewport, targetTop: 100, targetHeight: 50 })).toBe(500);
    expect(getTranscriptRevealOffset({ ...viewport, targetTop: 350, targetHeight: 50 })).toBe(500);
  });

  test('reveals a row above with only the required upward scroll', () => {
    expect(getTranscriptRevealOffset({ ...viewport, targetTop: 60, targetHeight: 80 })).toBe(460);
  });

  test('reveals a row below with only the required downward scroll', () => {
    expect(getTranscriptRevealOffset({ ...viewport, targetTop: 380, targetHeight: 80 })).toBe(560);
  });

  test('keeps a row that already covers the viewport in place', () => {
    expect(getTranscriptRevealOffset({ ...viewport, targetTop: 80, targetHeight: 400 })).toBe(500);
  });

  test('does not request a negative scroll offset', () => {
    expect(getTranscriptRevealOffset({ ...viewport, scrollTop: 10, targetTop: 60, targetHeight: 80 })).toBe(0);
  });
});
