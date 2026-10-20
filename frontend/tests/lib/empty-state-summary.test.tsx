import { afterAll, afterEach, describe, expect, mock, test } from 'bun:test';
import type { ComponentProps, PropsWithChildren } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

// Tooltip positioning needs a browser. Keep the real summary component and button.
const originalTooltip = { ...await import('../../src/components/ui/tooltip') };
const TooltipContent = ({ children }: PropsWithChildren) => <>{children}</>;
mock.module('../../src/components/ui/tooltip', () => ({
  TooltipProvider: TooltipContent,
  Tooltip: TooltipContent,
  TooltipTrigger: TooltipContent,
  TooltipContent,
}));
afterAll(() => mock.module('../../src/components/ui/tooltip', () => originalTooltip));
const { EmptyStateSummary } = await import('../../src/components/EmptyStateSummary');

let renderer: ReactTestRenderer | undefined;

afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

async function show(props: ComponentProps<typeof EmptyStateSummary>) {
  await act(async () => {
    if (renderer) renderer.update(<EmptyStateSummary {...props} />);
    else renderer = create(<EmptyStateSummary {...props} />);
  });
  return renderer!.root;
}

describe('empty summary state', () => {
  test('renders the failure as an alert and labels the action Retry summary', async () => {
    const root = await show({ error: 'Summary request failed', hasModel: true, onGenerate() {} });

    expect(root.findByProps({ role: 'alert' }).children).toEqual(['Summary request failed']);
    expect(root.findByType('button').children).toContain('Retry summary');
  });

  test('Retry calls the supplied generation action once', async () => {
    const onGenerate = mock(() => {});
    const root = await show({ error: 'Summary request failed', hasModel: true, onGenerate });
    const button = root.findByType('button');

    expect(button.props.disabled).toBe(false);
    expect(onGenerate).not.toHaveBeenCalled();
    await act(async () => button.props.onClick());
    expect(onGenerate).toHaveBeenCalledTimes(1);
  });

  test('keeps the error across model changes until the parent clears it', async () => {
    const props = { error: 'Summary request failed', hasModel: true, onGenerate() {} };
    await show(props);
    const root = await show({ ...props, hasModel: false });

    expect(root.findByProps({ role: 'alert' }).children).toEqual(['Summary request failed']);
    expect(root.findByType('button').children).toContain('Retry summary');

    await show({ ...props, error: null });
    expect(root.findAllByProps({ role: 'alert' })).toHaveLength(0);
    expect(root.findByType('button').children).toContain('Generate Summary');
  });

  test.each([
    { hasModel: false, isGenerating: false },
    { hasModel: true, isGenerating: true },
  ])('disables the action with hasModel=$hasModel and isGenerating=$isGenerating', async (state) => {
    const root = await show({ ...state, error: 'Summary request failed', onGenerate() {} });

    expect(root.findByType('button').props.disabled).toBe(true);
    if (state.isGenerating) expect(root.findByType('button').children).toContain('Generating...');
  });
});
