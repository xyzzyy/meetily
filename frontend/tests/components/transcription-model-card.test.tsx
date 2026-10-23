import React from 'react';
import { expect, mock, test } from 'bun:test';
import { create } from 'react-test-renderer';
import { TranscriptionModelCard, modelCardStatus } from '../../src/components/TranscriptionModelCard';

test('maps every engine status to one card status', () => {
  expect(modelCardStatus('Available')).toEqual({ kind: 'available' });
  expect(modelCardStatus('Missing', { partial: true })).toEqual({ kind: 'missing', partial: true });
  expect(modelCardStatus({ Error: 'offline' })).toEqual({ kind: 'error', message: 'offline', partial: undefined });
  expect(modelCardStatus({ Corrupted: { file_size: 1 } })).toEqual({ kind: 'corrupted' });
  expect(modelCardStatus({ Downloading: { progress: 30 } }, { detail: progress => `${progress} of 100` }))
    .toMatchObject({ kind: 'downloading', progress: 30, cancelling: false, detail: '30 of 100' });
  // A pending cancellation keeps the transfer row even before progress arrives.
  expect(modelCardStatus('Missing', { cancelling: true })).toMatchObject({ kind: 'downloading', progress: 0, cancelling: true });
});

function render(props: Partial<React.ComponentProps<typeof TranscriptionModelCard>>) {
  const handlers = { onSelect: mock(() => {}), onDownload: mock(() => {}), onCancel: mock(() => {}), onDelete: mock(() => {}) };
  const card = create(
    <TranscriptionModelCard displayName="Model" selected={false} status={{ kind: 'available' }} {...handlers} {...props} />
  );
  const buttons = () => card.root.findAllByType('button');
  const text = () => JSON.stringify(card.toJSON());
  return { card, buttons, text, handlers };
}

test('the selection area is a keyboard-reachable button that reports its state', () => {
  const { buttons, text } = render({ selected: true });
  const select = buttons()[0];
  expect(select.props.type).toBe('button');
  expect(select.props.disabled).toBe(false);
  expect(select.props['aria-pressed']).toBe(true);
  expect(text()).toContain('Selected');
  expect(text()).toContain('Ready');
});

test('a model that is not installed cannot be selected', () => {
  const { buttons } = render({ status: { kind: 'missing' } });
  expect(buttons()[0].props.disabled).toBe(true);
  expect(buttons().some(button => button.props.children?.[1] === 'Download')).toBe(true);
});

test('the delete control is always present and explains a selected model', () => {
  const unselected = render({});
  expect(unselected.buttons().some(button => button.props.title === 'Delete model')).toBe(true);
  const chosen = render({ selected: true });
  const remove = chosen.buttons().find(button => button.props.title === 'Select another model before deleting this one')!;
  expect(remove.props['aria-disabled']).toBe(true);
});

test('partial and failed downloads offer the same recovery controls', () => {
  const partial = render({ status: { kind: 'missing', partial: true } });
  expect(partial.buttons().some(button => button.props.children?.[1] === 'Resume')).toBe(true);
  expect(partial.buttons().some(button => button.props.title === 'Delete partial download')).toBe(true);
  const failed = render({ status: { kind: 'error', message: 'Checksum mismatch' } });
  expect(failed.buttons().some(button => button.props.children?.[1] === 'Retry')).toBe(true);
  expect(failed.text()).toContain('Checksum mismatch');
});

test('a cancellation request replaces Cancel with its pending state', () => {
  const active = render({ status: { kind: 'downloading', progress: 40, detail: '40 / 100 MB' } });
  expect(active.buttons().some(button => button.props.title === 'Cancel download')).toBe(true);
  expect(active.text()).toContain('40 / 100 MB');
  const cancelling = render({ status: { kind: 'downloading', progress: 40, cancelling: true } });
  expect(cancelling.buttons().some(button => button.props.title === 'Cancel download')).toBe(false);
  expect(cancelling.text()).toContain('Cancellation requested');
});
