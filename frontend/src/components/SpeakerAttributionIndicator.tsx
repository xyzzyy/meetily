'use client';

import { Check, Sparkles } from 'lucide-react';
import { Tooltip, TooltipContent, TooltipTrigger } from './ui/tooltip';

/** Human confirmation is separate from a model's confidence or final status. */
export function SpeakerAttributionIndicator({ verified, description }: {
  verified: boolean;
  description?: string;
}) {
  const label = verified ? 'Speaker confirmed by you' : 'Speaker assigned automatically';
  const Icon = verified ? Check : Sparkles;
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span
          role="img"
          aria-label={label}
          tabIndex={0}
          className={`inline-flex shrink-0 items-center rounded-sm focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 ${verified ? 'text-emerald-600 dark:text-emerald-400' : 'text-muted-foreground'}`}
        >
          <Icon className="h-3 w-3" aria-hidden="true" />
        </span>
      </TooltipTrigger>
      <TooltipContent>{description || label}</TooltipContent>
    </Tooltip>
  );
}
