import React from 'react';
import { cn } from '@/lib/utils';

type SettingsStatusBadgeTone = 'beta' | 'local' | 'provisional' | 'final' | 'fallback' | 'review' | 'unavailable';

const toneClassName: Record<SettingsStatusBadgeTone, string> = {
  beta: 'bg-yellow-100 dark:bg-yellow-900/50 text-yellow-800 dark:text-yellow-200',
  local: 'bg-blue-100 dark:bg-blue-900/50 text-blue-800 dark:text-blue-200',
  provisional: 'bg-amber-100 dark:bg-amber-900/50 text-amber-800 dark:text-amber-200',
  final: 'bg-green-100 dark:bg-green-900/50 text-green-800 dark:text-green-200',
  fallback: 'bg-orange-100 dark:bg-orange-900/50 text-orange-800 dark:text-orange-200',
  review: 'bg-purple-100 dark:bg-purple-900/50 text-purple-800 dark:text-purple-200',
  unavailable: 'bg-muted text-muted-foreground',
};

interface SettingsStatusBadgeProps {
  tone: SettingsStatusBadgeTone;
  children: React.ReactNode;
}

export function SettingsStatusBadge({ tone, children }: SettingsStatusBadgeProps) {
  return (
    <span className={cn('inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium', toneClassName[tone])}>
      {children}
    </span>
  );
}
