import React from 'react';
import { AlertCircle, CheckCircle2, ExternalLink, Loader2, RotateCw, XCircle } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import type { PermissionRowProps, PermissionStatus } from '@/types/onboarding';

const statusBadges: Partial<Record<PermissionStatus, { label: string; icon: typeof CheckCircle2; className: string }>> = {
  authorized: {
    label: 'Access granted',
    icon: CheckCircle2,
    className: 'bg-green-50 text-green-700 ring-green-200 dark:bg-green-500/10 dark:text-green-400 dark:ring-green-500/20',
  },
  denied: {
    label: 'Access denied',
    icon: XCircle,
    className: 'bg-red-50 text-red-700 ring-red-200 dark:bg-red-500/10 dark:text-red-400 dark:ring-red-500/20',
  },
  unverified: {
    label: 'Not verified',
    icon: AlertCircle,
    className: 'bg-amber-50 text-amber-800 ring-amber-200 dark:bg-amber-500/10 dark:text-amber-300 dark:ring-amber-500/20',
  },
};

export function PermissionRow({
  icon,
  title,
  description,
  status,
  isPending = false,
  disabled = false,
  onAction,
  onRetry,
}: PermissionRowProps) {
  const isAuthorized = status === 'authorized';
  const isDenied = status === 'denied';
  const needsSettings = isDenied || status === 'unverified';
  const isBusy = isPending || disabled;
  const badge = statusBadges[status];
  const BadgeIcon = badge?.icon;

  return (
    <div
      className={cn(
        'flex items-start gap-4 rounded-xl border bg-white p-4 transition-colors duration-200 dark:bg-ink-900 sm:p-5',
        isDenied ? 'border-red-200 dark:border-red-500/25' : 'border-neutral-200 dark:border-ink-800'
      )}
    >
      <div
        className={cn(
          'flex size-10 flex-shrink-0 items-center justify-center rounded-full',
          isAuthorized
            ? 'bg-green-50 text-green-600 dark:bg-green-500/10 dark:text-green-400'
            : isDenied
              ? 'bg-red-50 text-red-600 dark:bg-red-500/10 dark:text-red-400'
              : 'bg-neutral-100 text-neutral-600 dark:bg-ink-800 dark:text-ink-300'
        )}
      >
        {icon}
      </div>

      <div className="min-w-0 flex-1">
        <div className="flex min-h-10 flex-wrap items-center justify-between gap-x-3 gap-y-1">
          <h3 className="font-medium text-neutral-900 dark:text-ink-100">{title}</h3>
          {badge && BadgeIcon && (
            <span
              className={cn(
                'inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium ring-1 ring-inset',
                badge.className
              )}
            >
              <BadgeIcon className="h-3.5 w-3.5" aria-hidden="true" />
              {badge.label}
            </span>
          )}
        </div>

        <p className="text-sm leading-relaxed text-neutral-600 dark:text-ink-300">{description}</p>

        {!isAuthorized && (
          <div className="mt-3 flex flex-wrap gap-2">
            {needsSettings ? (
              <>
                <Button variant="outline" size="sm" onClick={onAction} disabled={isBusy}>
                  <ExternalLink aria-hidden="true" />
                  Open Settings
                </Button>
                {onRetry && (
                  <Button variant="outline" size="sm" onClick={onRetry} disabled={isBusy}>
                    {isPending ? <Loader2 className="animate-spin" aria-hidden="true" /> : <RotateCw aria-hidden="true" />}
                    {isPending ? 'Checking...' : 'Check Again'}
                  </Button>
                )}
              </>
            ) : (
              <Button size="sm" onClick={onAction} disabled={isBusy} className="min-w-[96px]">
                {isPending && <Loader2 className="animate-spin" aria-hidden="true" />}
                {isPending ? 'Checking...' : 'Enable'}
              </Button>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
