import React, { useRef } from 'react';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogTitle } from '@/components/ui/dialog';

interface ConfirmationModalProps {
  onConfirm: () => void;
  onCancel: () => void;
  text: string;
  isOpen: boolean;
  returnFocusRef?: React.RefObject<HTMLElement>;
}

export function ConfirmationModal({ onConfirm, onCancel, text, isOpen, returnFocusRef }: ConfirmationModalProps) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const opener = useRef<HTMLElement | null>(null);
  const confirmed = useRef(false);
  return (
    <Dialog open={isOpen} onOpenChange={open => { if (!open) onCancel(); }}>
      <DialogContent onOpenAutoFocus={event => {
        event.preventDefault();
        opener.current = document.activeElement as HTMLElement;
        confirmed.current = false;
        cancelRef.current?.focus();
      }} onCloseAutoFocus={event => {
        event.preventDefault();
        // Confirmation can remove the opener after an asynchronous delete.
        const target = confirmed.current && returnFocusRef?.current
          ? returnFocusRef.current : opener.current?.isConnected ? opener.current : returnFocusRef?.current;
        target?.focus();
      }}>
        <DialogTitle>Confirm Delete</DialogTitle>
        <DialogDescription>{text}</DialogDescription>
        <DialogFooter>
          <button ref={cancelRef} type="button" onClick={onCancel}
            className="px-4 py-2 text-gray-600 dark:text-ink-300 hover:bg-gray-100 dark:hover:bg-ink-800 rounded-md">Cancel</button>
          <button type="button" onClick={() => { confirmed.current = true; onConfirm(); }}
            className="px-4 py-2 bg-red-600 text-white hover:bg-red-700 rounded-md">Delete</button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
