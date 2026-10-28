'use client';

import { useCallback, useRef, useReducer, startTransition, useEffect, useState, memo } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useAutoScroll } from "@/hooks/useAutoScroll";
import { useTranscriptStreaming } from "@/hooks/useTranscriptStreaming";
import { SpeakerAttributionIndicator } from "./SpeakerAttributionIndicator";
import { ConfidenceIndicator } from "./ConfidenceIndicator";
import { Tooltip, TooltipContent, TooltipTrigger } from "./ui/tooltip";
import { RecordingStatusBar } from "./RecordingStatusBar";
import { motion, AnimatePresence } from "framer-motion";
import { TranscriptSegmentData } from "@/types";
import type { DiarizationStatus } from "@/types/diarization";
import { findTargetSegmentIndex, getTranscriptRevealOffset, targetMayBeUnloaded } from "@/lib/transcript-navigation";

export interface VirtualizedTranscriptViewProps {
    /** Transcript segments to display */
    segments: TranscriptSegmentData[];
    /** Whether recording is in progress */
    isRecording?: boolean;
    /** Whether recording is paused */
    isPaused?: boolean;
    /** Whether processing/finalizing transcription */
    isProcessing?: boolean;
    /** Whether stopping */
    isStopping?: boolean;
    /** Enable streaming effect for latest segment */
    enableStreaming?: boolean;
    /** Show confidence indicators */
    showConfidence?: boolean;
    /** Completely disable auto-scroll behavior (for meeting details page) */
    disableAutoScroll?: boolean;
    /** When set, scrolls to the segment whose timestamp contains this time (seconds) */
    scrollToTime?: number;
    /** Increment to trigger scrollToTime even if time value is the same */
    scrollToKey?: number;

    // Pagination props (infinite scroll)
    hasMore?: boolean;
    isLoadingMore?: boolean;
    totalCount?: number;
    loadedCount?: number;
    onLoadMore?: () => void;
    selectedSegmentIds?: Set<string>;
    onToggleSegment?: (id: string) => void;
}

// Threshold for enabling virtualization (below this, use simple rendering)
const VIRTUALIZATION_THRESHOLD = 10;
// Page requests a reference jump may make without the transcript growing before it settles
const MAX_JUMP_LOAD_ATTEMPTS = 10;

// Helper function to format seconds as recording-relative time [MM:SS]
function formatRecordingTime(seconds: number | undefined): string {
    if (seconds === undefined) return '[--:--]';

    const totalSeconds = Math.floor(seconds);
    const minutes = Math.floor(totalSeconds / 60);
    const secs = totalSeconds % 60;

    return `[${minutes.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}]`;
}

// Helper function to remove filler words and repetitions
function cleanStopWords(text: string): string {
    const stopWords = ['uh', 'um', 'er', 'ah', 'hmm', 'hm', 'eh', 'oh'];

    let cleanedText = text;
    stopWords.forEach(word => {
        const pattern = new RegExp(`\\b${word}\\b[,\\s]*`, 'gi');
        cleanedText = cleanedText.replace(pattern, ' ');
    });

    return cleanedText.replace(/\s+/g, ' ').trim();
}

type VisibleDiarizationStatus = Exclude<DiarizationStatus, 'none'>;

const DIARIZATION_STATUS_BADGES: Record<VisibleDiarizationStatus, { label: string; toneClassName: string }> = {
    provisional: { label: 'Provisional', toneClassName: 'bg-amber-100 dark:bg-amber-500/15 text-amber-800 dark:text-amber-200' },
    final: { label: 'Final', toneClassName: 'bg-green-100 dark:bg-green-500/15 text-green-800 dark:text-green-200' },
    needs_review: { label: 'Needs review', toneClassName: 'bg-purple-100 dark:bg-purple-500/15 text-purple-800 dark:text-purple-200' },
    fallback_to_live: { label: 'Live fallback', toneClassName: 'bg-orange-100 dark:bg-orange-500/15 text-orange-800 dark:text-orange-200' },
    failed: { label: 'Failed', toneClassName: 'bg-destructive/15 text-destructive' },
};

// Memoized transcript segment component
const TranscriptSegment = memo(function TranscriptSegment({
    id,
    timestamp,
    endTime,
    text,
    confidence,
    speakerLabel,
    speakerColor,
    isOverlap,
    diarizationStatus,
    diarizationMethod,
    isStreaming,
    showConfidence,
    selected,
    onToggle,
    highlighted,
}: {
    id: string;
    timestamp: number;
    endTime?: number;
    text: string;
    confidence?: number;
    speakerLabel?: string | null;
    speakerColor?: string | null;
    isOverlap?: boolean | null;
    diarizationStatus?: DiarizationStatus | null;
    diarizationMethod?: string | null;
    isStreaming: boolean;
    showConfidence: boolean;
    selected?: boolean;
    onToggle?: (id: string) => void;
    highlighted?: boolean;
}) {
    const displayText = cleanStopWords(text) || (text.trim() === '' ? '[Silence]' : text);
    const duration = endTime !== undefined && endTime >= timestamp
        ? endTime - timestamp
        : undefined;
    const hasConfidence = confidence !== undefined && showConfidence;
    const hasTooltip = duration !== undefined || hasConfidence;
    const timestampLabel = (
        // Focusable only as a tooltip trigger, so keyboard users can open it.
        <span
            className="text-xs text-gray-400 mt-1 flex-shrink-0 min-w-[50px]"
            tabIndex={hasTooltip ? 0 : undefined}
        >
            {formatRecordingTime(timestamp)}
        </span>
    );
    const visibleSpeakerLabel = isOverlap ? 'Multiple speakers' : speakerLabel?.trim();
    const statusBadge =
        diarizationStatus && diarizationStatus !== 'none' && diarizationStatus !== 'final'
            ? DIARIZATION_STATUS_BADGES[diarizationStatus]
            : null;

    return (
        <div
            id={`segment-${id}`}
            data-highlighted={highlighted || undefined}
            className={`relative mb-3 rounded-sm transition-colors duration-200 motion-reduce:transition-none ${
                highlighted ? 'bg-blue-50 dark:bg-blue-400/10' : ''
            }`}
        >
            <span
                aria-hidden="true"
                className={`pointer-events-none absolute inset-y-0 -left-2 w-0.5 rounded-full bg-blue-500 transition-opacity duration-200 motion-reduce:transition-none ${highlighted ? 'opacity-100' : 'opacity-0'}`}
            />
            <div className="flex items-start gap-2">
                {onToggle && (
                    <input
                        type="checkbox"
                        aria-label="Select transcript row"
                        className="mt-1"
                        checked={selected}
                        onChange={() => onToggle(id)}
                    />
                )}
                {hasTooltip ? (
                    <Tooltip>
                        <TooltipTrigger asChild>
                            {timestampLabel}
                        </TooltipTrigger>
                        <TooltipContent>
                            <div className="flex items-center gap-2">
                                {duration !== undefined && (
                                    <span>{duration.toFixed(1)}s</span>
                                )}
                                {hasConfidence && (
                                    <ConfidenceIndicator confidence={confidence} showIndicator={showConfidence} />
                                )}
                            </div>
                        </TooltipContent>
                    </Tooltip>
                ) : timestampLabel}
                <div className="flex-1">
                    {(visibleSpeakerLabel || statusBadge) && (
                        <div className="mb-1 flex items-center gap-2 text-xs">
                            {visibleSpeakerLabel && (
                                <span
                                    className="font-medium"
                                    style={{ color: speakerColor || (isOverlap ? '#f97316' : undefined) }}
                                >
                                    {visibleSpeakerLabel}
                                </span>
                            )}
                            {visibleSpeakerLabel && !isOverlap && visibleSpeakerLabel !== 'Unknown speaker' && (
                                <SpeakerAttributionIndicator verified={diarizationMethod === 'manual'} />
                            )}
                            {statusBadge && (
                                <span className={`rounded-full px-2 py-0.5 font-medium ${statusBadge.toneClassName}`}>
                                    {statusBadge.label}
                                </span>
                            )}
                        </div>
                    )}
                    {isStreaming ? (
                        <div className="bg-gray-100 dark:bg-ink-800 border border-gray-200 dark:border-ink-800 rounded-lg px-3 py-2">
                            <p className="text-base text-gray-800 dark:text-ink-100 leading-relaxed">{displayText}</p>
                        </div>
                    ) : (
                        <p className="text-base text-gray-800 dark:text-ink-100 leading-relaxed">{displayText}</p>
                    )}
                </div>
            </div>
        </div>
    );
});

export const VirtualizedTranscriptView: React.FC<VirtualizedTranscriptViewProps> = ({
    segments,
    isRecording = false,
    isPaused = false,
    isProcessing = false,
    isStopping = false,
    enableStreaming = false,
    showConfidence = true,
    disableAutoScroll = false,
    scrollToTime,
    scrollToKey,
    hasMore = false,
    isLoadingMore = false,
    totalCount = 0,
    loadedCount = 0,
    onLoadMore,
    selectedSegmentIds,
    onToggleSegment,
}) => {
    // Create scroll ref first - shared between virtualizer and auto-scroll hook
    const scrollRef = useRef<HTMLDivElement>(null);
    // Ref for infinite scroll trigger element
    const loadMoreTriggerRef = useRef<HTMLDivElement>(null);

    // Force re-render without flushSync (avoids React warning)
    const [, rerender] = useReducer((x: number) => x + 1, 0);

    // Setup virtualizer for efficient rendering of large lists
    const virtualizer = useVirtualizer({
        count: segments.length,
        getScrollElement: () => scrollRef.current,
        estimateSize: () => 60, // Estimated height per segment
        overscan: 10, // Render extra items above/below viewport
        onChange: () => {
            startTransition(() => {
                rerender();
            });
        },
    });

    // Custom hook for auto-scrolling (supports both virtualized and non-virtualized)
    useAutoScroll({
        scrollRef,
        segments,
        isRecording,
        isPaused,
        virtualizer,
        virtualizationThreshold: VIRTUALIZATION_THRESHOLD,
        disableAutoScroll,
    });

    // Streaming text effect hook (typewriter animation for new transcripts)
    const { streamingSegmentId, getDisplayText } = useTranscriptStreaming(
        segments,
        isRecording,
        enableStreaming
    );

    // Infinite scroll: IntersectionObserver to trigger loading more
    useEffect(() => {
        if (!onLoadMore || !hasMore || isLoadingMore || isRecording || segments.length === 0) {
            return;
        }

        const triggerElement = loadMoreTriggerRef.current;
        if (!triggerElement) return;

        const observer = new IntersectionObserver(
            (entries) => {
                if (entries[0].isIntersecting && hasMore && !isLoadingMore) {
                    onLoadMore();
                }
            },
            {
                root: null,
                rootMargin: '100px',
                threshold: 0,
            }
        );

        observer.observe(triggerElement);

        return () => observer.disconnect();
    }, [hasMore, isLoadingMore, onLoadMore, isRecording, segments.length]);

    // Scroll-based fallback for fast scrolling
    useEffect(() => {
        if (!onLoadMore || !hasMore || isLoadingMore || isRecording) return;

        const scrollElement = scrollRef.current;
        if (!scrollElement) return;

        let ticking = false;

        const handleScroll = () => {
            if (ticking || isLoadingMore || !hasMore) return;

            ticking = true;
            requestAnimationFrame(() => {
                const { scrollTop, scrollHeight, clientHeight } = scrollElement;
                const scrollBottom = scrollHeight - scrollTop - clientHeight;

                // Trigger load when within 200px of bottom
                if (scrollBottom < 200 && hasMore && !isLoadingMore) {
                    onLoadMore();
                }
                ticking = false;
            });
        };

        scrollElement.addEventListener('scroll', handleScroll, { passive: true });
        return () => scrollElement.removeEventListener('scroll', handleScroll);
    }, [onLoadMore, hasMore, isLoadingMore, isRecording]);

    // Use simple rendering for small lists, virtualization for large lists
    const useVirtualization = segments.length >= VIRTUALIZATION_THRESHOLD;

    // Jump to a transcript reference when scrollToKey changes, paging in segments if needed
    const lastHandledKeyRef = useRef(0);
    const pendingKeyRef = useRef(0);
    const loadAttemptsRef = useRef({ key: 0, loaded: 0, count: 0 });
    const [loadRecheck, setLoadRecheck] = useState(0);
    const [highlight, setHighlight] = useState<{ id: string; key: number } | null>(null);
    const highlightedSegmentId = highlight?.id ?? null;

    useEffect(() => {
        if (!highlight) return;
        const timer = setTimeout(() => setHighlight(null), 1000);
        return () => clearTimeout(timer);
    }, [highlight]);

    useEffect(() => {
        if (scrollToTime === undefined || !scrollToKey) return;
        if (segments.length === 0) {
            // Transcript was reset: drop any in-flight jump
            pendingKeyRef.current = 0;
            return;
        }
        if (scrollToKey === lastHandledKeyRef.current && scrollToKey !== pendingKeyRef.current) return;
        lastHandledKeyRef.current = scrollToKey;

        const attempts = loadAttemptsRef.current;
        if (attempts.key !== scrollToKey || segments.length > attempts.loaded) {
            loadAttemptsRef.current = { key: scrollToKey, loaded: segments.length, count: 0 };
        }

        if (
            targetMayBeUnloaded(segments, scrollToTime) && hasMore && onLoadMore &&
            loadAttemptsRef.current.count < MAX_JUMP_LOAD_ATTEMPTS
        ) {
            pendingKeyRef.current = scrollToKey;
            // An in-flight page re-runs this effect when it finishes
            if (isLoadingMore) return;
            loadAttemptsRef.current.count += 1;
            onLoadMore();
            // loadMore ignores calls less than 100ms apart; re-check in case this one was dropped
            const timer = setTimeout(() => setLoadRecheck(n => n + 1), 150);
            return () => clearTimeout(timer);
        }

        // Target is loaded, or no more pages can be loaded: jump to the best loaded segment
        pendingKeyRef.current = 0;
        const targetIndex = findTargetSegmentIndex(segments, scrollToTime);
        const targetSegment = segments[targetIndex];
        if (!targetSegment) return;
        setHighlight({ id: targetSegment.id, key: scrollToKey });
        const container = scrollRef.current;
        const target = document.getElementById(`segment-${targetSegment.id}`);
        if (container && target && container.contains(target)) {
            const viewport = container.getBoundingClientRect();
            const row = target.getBoundingClientRect();
            const top = getTranscriptRevealOffset({
                scrollTop: container.scrollTop,
                viewportTop: viewport.top + container.clientTop,
                viewportHeight: container.clientHeight,
                targetTop: row.top,
                targetHeight: row.height,
            });
            if (top !== container.scrollTop) container.scrollTo({ top, behavior: 'instant' });
        } else if (useVirtualization) {
            // The height is unknown until mounting. Keep the timestamp visible even for long rows.
            virtualizer.scrollToIndex(targetIndex, { align: 'start' });
        }
    }, [scrollToTime, scrollToKey, segments, hasMore, isLoadingMore, onLoadMore, useVirtualization, virtualizer, loadRecheck]);

    return (
        <div ref={scrollRef} data-transcript-scroll-container className="flex flex-col h-full overflow-y-auto px-4 py-2">
            {/* Recording Status Bar - Sticky at top, always visible when recording */}
            <AnimatePresence>
                {isRecording && (
                    <div className="sticky top-0 z-10 bg-white dark:bg-ink-900 pb-2">
                        <RecordingStatusBar isPaused={isPaused} />
                    </div>
                )}
            </AnimatePresence>

            {/* Content - add padding when recording to prevent overlap */}
            <div className={isRecording ? 'pt-2' : ''}>
            {segments.length === 0 ? (
                // Empty state
                <motion.div
                    initial={{ opacity: 0 }}
                    animate={{ opacity: 1 }}
                    className="text-center text-gray-500 dark:text-ink-400 mt-8"
                >
                    {isRecording ? (
                        <>
                            <div className="flex items-center justify-center mb-3">
                                <div className={`w-3 h-3 rounded-full ${isPaused ? 'bg-orange-500' : 'bg-blue-500 animate-pulse'}`}></div>
                            </div>
                            <p className="text-sm text-gray-600 dark:text-ink-300">
                                {isPaused ? 'Recording paused' : 'Listening for speech...'}
                            </p>
                            <p className="text-xs mt-1 text-gray-400">
                                {isPaused ? 'Click resume to continue recording' : 'Speak to see live transcription'}
                            </p>
                        </>
                    ) : (
                        <>
                            <p className="text-lg font-semibold">Welcome to meetily!</p>
                            <p className="text-xs mt-1">Start recording to see live transcription</p>
                        </>
                    )}
                </motion.div>
            ) : useVirtualization ? (
                // Virtualized rendering for large lists
                <>
                    <div
                        style={{
                            height: virtualizer.getTotalSize(),
                            width: "100%",
                            position: "relative",
                        }}
                    >
                        {virtualizer.getVirtualItems().map((virtualRow) => {
                            const segment = segments[virtualRow.index];
                            const isStreaming = streamingSegmentId === segment.id;

                            return (
                                <div
                                    key={segment.id}
                                    data-index={virtualRow.index}
                                    ref={virtualizer.measureElement}
                                    style={{
                                        position: "absolute",
                                        top: 0,
                                        left: 0,
                                        width: "100%",
                                        transform: `translateY(${virtualRow.start}px)`,
                                    }}
                                >
                                    <TranscriptSegment
                                        id={segment.id}
                                        timestamp={segment.timestamp}
                                        endTime={segment.endTime}
                                        text={getDisplayText(segment)}
                                        confidence={segment.confidence}
                                        speakerLabel={segment.speaker_label}
                                        speakerColor={segment.speaker_color}
                                        isOverlap={segment.is_overlap}
                                        diarizationStatus={segment.diarization_status}
                                        diarizationMethod={segment.diarization_method}
                                        isStreaming={isStreaming}
                                        showConfidence={showConfidence}
                                        selected={selectedSegmentIds?.has(segment.id)}
                                        onToggle={onToggleSegment}
                                        highlighted={highlightedSegmentId === segment.id}
                                    />
                                </div>
                            );
                        })}
                    </div>

                    {/* Infinite scroll trigger and loading indicator */}
                    {(hasMore || isLoadingMore) && !isRecording && segments.length > 0 && (
                        <div ref={loadMoreTriggerRef} className="flex justify-center items-center py-4 mt-2">
                            {isLoadingMore ? (
                                <div className="flex items-center gap-2 text-gray-500 dark:text-ink-400">
                                    <div className="w-4 h-4 border-2 border-gray-300 dark:border-ink-700 border-t-gray-600 dark:border-t-ink-300 rounded-full animate-spin" />
                                    <span className="text-sm">Loading more...</span>
                                </div>
                            ) : hasMore && totalCount > 0 ? (
                                <span className="text-sm text-gray-400">
                                    Showing {loadedCount} of {totalCount} segments
                                </span>
                            ) : null}
                        </div>
                    )}

                    {/* Listening indicator when recording */}
                    {!isStopping && isRecording && !isPaused && !isProcessing && segments.length > 0 && (
                        <motion.div
                            initial={{ opacity: 0 }}
                            animate={{ opacity: 1 }}
                            exit={{ opacity: 0 }}
                            className="flex items-center gap-2 mt-4 text-gray-500 dark:text-ink-400"
                        >
                            <div className="w-2 h-2 bg-blue-500 rounded-full animate-pulse"></div>
                            <span className="text-sm">Listening...</span>
                        </motion.div>
                    )}
                </>
            ) : (
                // Simple rendering for small lists (better animations)
                <>
                    <div className="space-y-1">
                        {segments.map((segment) => {
                            const isStreaming = streamingSegmentId === segment.id;

                            return (
                                <motion.div
                                    key={segment.id}
                                    initial={{ opacity: 0, y: 5 }}
                                    animate={{ opacity: 1, y: 0 }}
                                    transition={{ duration: 0.15 }}
                                >
                                    <TranscriptSegment
                                        id={segment.id}
                                        timestamp={segment.timestamp}
                                        endTime={segment.endTime}
                                        text={getDisplayText(segment)}
                                        confidence={segment.confidence}
                                        speakerLabel={segment.speaker_label}
                                        speakerColor={segment.speaker_color}
                                        isOverlap={segment.is_overlap}
                                        diarizationStatus={segment.diarization_status}
                                        diarizationMethod={segment.diarization_method}
                                        isStreaming={isStreaming}
                                        showConfidence={showConfidence}
                                        selected={selectedSegmentIds?.has(segment.id)}
                                        onToggle={onToggleSegment}
                                        highlighted={highlightedSegmentId === segment.id}
                                    />
                                </motion.div>
                            );
                        })}
                    </div>

                    {/* Infinite scroll trigger (for small lists that grow) */}
                    {(hasMore || isLoadingMore) && !isRecording && segments.length > 0 && (
                        <div ref={loadMoreTriggerRef} className="flex justify-center items-center py-4 mt-2">
                            {isLoadingMore ? (
                                <div className="flex items-center gap-2 text-gray-500 dark:text-ink-400">
                                    <div className="w-4 h-4 border-2 border-gray-300 dark:border-ink-700 border-t-gray-600 dark:border-t-ink-300 rounded-full animate-spin" />
                                    <span className="text-sm">Loading more...</span>
                                </div>
                            ) : hasMore && totalCount > 0 ? (
                                <span className="text-sm text-gray-400">
                                    Showing {loadedCount} of {totalCount} segments
                                </span>
                            ) : null}
                        </div>
                    )}

                    {/* Listening indicator when recording */}
                    {!isStopping && isRecording && !isPaused && !isProcessing && segments.length > 0 && (
                        <motion.div
                            initial={{ opacity: 0 }}
                            animate={{ opacity: 1 }}
                            exit={{ opacity: 0 }}
                            className="flex items-center gap-2 mt-4 text-gray-500 dark:text-ink-400"
                        >
                            <div className="w-2 h-2 bg-blue-500 rounded-full animate-pulse"></div>
                            <span className="text-sm">Listening...</span>
                        </motion.div>
                    )}
                </>
            )}
            </div>
        </div>
    );
};
