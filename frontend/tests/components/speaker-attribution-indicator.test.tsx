import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { TooltipProvider } from '../../src/components/ui/tooltip';
import { VirtualizedTranscriptView } from '../../src/components/VirtualizedTranscriptView';
import { SpeakerAttributionIndicator } from '../../src/components/SpeakerAttributionIndicator';

function transcript(method: string) {
  return renderToStaticMarkup(<TooltipProvider><VirtualizedTranscriptView
    enableStreaming={false}
    segments={[{ id: 't1', text: 'Hello', timestamp: 0, endTime: 1,
      speaker_label: 'Alice', speaker_id: 's1', diarization_status: 'final', diarization_method: method }]}
  /></TooltipProvider>);
}

describe('speaker attribution indicators', () => {
  test('a final automatic result does not claim human confirmation', () => {
    const html = transcript('fluid_audio_offline_0.12.4');
    expect(html).toContain('aria-label="Speaker assigned automatically"');
    expect(html).not.toContain('aria-label="Speaker confirmed by you"');
    expect(html).not.toContain('>Final<');
  });
  test('a manual assignment shows the confirmation check', () => {
    const html = transcript('manual');
    expect(html).toContain('aria-label="Speaker confirmed by you"');
    expect(html).toContain('lucide-check');
  });
  test('missing provenance stays automatic', () => {
    const html = renderToStaticMarkup(<TooltipProvider>
      <SpeakerAttributionIndicator verified={false} />
    </TooltipProvider>);
    expect(html).toContain('aria-label="Speaker assigned automatically"');
    expect(html).toContain('lucide-sparkles');
  });
});
