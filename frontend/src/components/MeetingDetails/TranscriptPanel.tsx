"use client";

import { Transcript, TranscriptSegmentData } from '@/types';
import { VirtualizedTranscriptView } from '@/components/VirtualizedTranscriptView';
import { TranscriptButtonGroup } from './TranscriptButtonGroup';
import { useMemo, useCallback, useState } from 'react';
import { useQueueJob, useQueueSnapshot } from '@/hooks/useQueueJobStatus';
import { QueueJob, cancelQueuedJob, enqueueTranscriptionJob } from '@/services/queueService';

interface TranscriptPanelProps {
  transcripts: Transcript[];
  customPrompt: string;
  onPromptChange: (value: string) => void;
  onCopyTranscript: () => void;
  onOpenMeetingFolder: () => Promise<void>;
  isRecording: boolean;
  disableAutoScroll?: boolean;

  // Optional pagination props (when using virtualization)
  usePagination?: boolean;
  segments?: TranscriptSegmentData[];
  hasMore?: boolean;
  isLoadingMore?: boolean;
  totalCount?: number;
  loadedCount?: number;
  onLoadMore?: () => void;

  // Retranscription props
  meetingId?: string;
  meetingFolderPath?: string | null;
  onRefetchTranscripts?: () => Promise<void>;
}

function TranscriptionStatusBanner({
  job,
  manualPause,
  onRetry,
  isRetrying,
}: {
  job: QueueJob;
  manualPause: boolean;
  onRetry: () => void;
  isRetrying: boolean;
}) {
  if (job.status === 'Done') return null;

  const isActive = job.status === 'InProgress';
  const isPaused = job.status === 'Paused';
  const isFailed = job.status === 'Failed';
  const isPending = job.status === 'Pending';

  const icon = isActive ? (
    <span className="w-2.5 h-2.5 rounded-full bg-blue-500 animate-pulse flex-shrink-0" />
  ) : isPaused ? (
    <span className="w-2.5 h-2.5 rounded-full bg-orange-400 flex-shrink-0" />
  ) : isFailed ? (
    <span className="text-red-500 text-base leading-none flex-shrink-0">✕</span>
  ) : (
    <span className="w-2.5 h-2.5 rounded-full border-2 border-yellow-500 flex-shrink-0" />
  );

  const headline = isActive
    ? job.phase === 'Summarising'
      ? 'Generating summary…'
      : 'Transcribing your recording…'
    : isPaused
    ? 'Transcription paused'
    : isFailed
    ? 'Transcription failed'
    : 'Waiting to transcribe…';

  const detail = isActive
    ? job.phase === 'Summarising'
      ? 'The AI summary will appear here once complete.'
      : 'The transcript will appear here once Whisper finishes processing your audio.'
    : isPaused
    ? manualPause
      ? 'Transcription is paused. Click Resume in the queue indicator to continue.'
      : 'Transcription is paused — it will resume automatically when the system is ready.'
    : isFailed
    ? 'Something went wrong. You can retry transcription below.'
    : isPending
    ? 'Your recording is queued and will start transcribing shortly.'
    : '';

  const containerColor = isFailed
    ? 'bg-red-50 border-red-200'
    : isPaused
    ? 'bg-orange-50 border-orange-200'
    : isActive
    ? 'bg-blue-50 border-blue-200'
    : 'bg-yellow-50 border-yellow-200';

  const headlineColor = isFailed
    ? 'text-red-800'
    : isPaused
    ? 'text-orange-800'
    : isActive
    ? 'text-blue-800'
    : 'text-yellow-800';

  const detailColor = isFailed
    ? 'text-red-600'
    : isPaused
    ? 'text-orange-600'
    : isActive
    ? 'text-blue-600'
    : 'text-yellow-600';

  return (
    <div role="status" aria-live="polite" className="flex flex-col items-center justify-center h-full px-6 text-center">
      <div className={`w-full max-w-xs rounded-xl border p-5 flex flex-col items-center gap-3 ${containerColor}`}>
        <div className="flex items-center gap-2">
          {icon}
          <span className={`text-sm font-semibold ${headlineColor}`}>{headline}</span>
        </div>
        {detail && (
          <p className={`text-xs leading-relaxed ${detailColor}`}>{detail}</p>
        )}
        {isFailed && (
          <button
            onClick={onRetry}
            disabled={isRetrying}
            className="mt-1 px-3 py-1.5 rounded-md bg-red-100 hover:bg-red-200 text-red-800 text-xs font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isRetrying ? 'Retrying…' : 'Retry transcription'}
          </button>
        )}
      </div>
    </div>
  );
}

export function TranscriptPanel({
  transcripts,
  customPrompt,
  onPromptChange,
  onCopyTranscript,
  onOpenMeetingFolder,
  isRecording,
  disableAutoScroll = false,
  usePagination = false,
  segments,
  hasMore,
  isLoadingMore,
  totalCount,
  loadedCount,
  onLoadMore,
  meetingId,
  meetingFolderPath,
  onRefetchTranscripts,
}: TranscriptPanelProps) {
  const queueJob = useQueueJob(meetingId);
  const queueSnapshot = useQueueSnapshot();
  const queueManualPause = queueSnapshot.manual_pause_all;
  const [isRetrying, setIsRetrying] = useState(false);

  const handleRetry = useCallback(async () => {
    if (!queueJob) return;
    setIsRetrying(true);
    try {
      await cancelQueuedJob(queueJob.meeting_id);
      await enqueueTranscriptionJob(queueJob.meeting_id, queueJob.audio_path);
    } catch (e) {
      console.error('Failed to retry transcription job:', e);
    } finally {
      setIsRetrying(false);
    }
  }, [queueJob]);

  const convertedSegments = useMemo(() => {
    if (usePagination && segments) {
      return segments;
    }
    return transcripts.map(t => ({
      id: t.id,
      timestamp: t.audio_start_time ?? 0,
      endTime: t.audio_end_time,
      text: t.text,
      confidence: t.confidence,
      speaker: t.speaker,
    }));
  }, [transcripts, usePagination, segments]);

  const isEmpty = convertedSegments.length === 0;

  return (
    <div className="hidden md:flex md:w-1/4 lg:w-1/3 min-w-0 border-r border-gray-200 bg-white flex-col relative shrink-0">
      {/* Title area */}
      <div className="p-4 border-b border-gray-200">
        <TranscriptButtonGroup
          transcriptCount={usePagination ? (totalCount ?? convertedSegments.length) : (transcripts?.length || 0)}
          onCopyTranscript={onCopyTranscript}
          onOpenMeetingFolder={onOpenMeetingFolder}
          meetingId={meetingId}
          meetingFolderPath={meetingFolderPath}
          onRefetchTranscripts={onRefetchTranscripts}
        />
      </div>

      {/* Transcript content */}
      <div className="flex-1 overflow-hidden pb-4">
        {isEmpty && !isRecording && queueJob && queueJob.status !== 'Done' ? (
          <TranscriptionStatusBanner job={queueJob} manualPause={queueManualPause} onRetry={handleRetry} isRetrying={isRetrying} />
        ) : (
          <VirtualizedTranscriptView
            segments={convertedSegments}
            isRecording={isRecording}
            isPaused={false}
            isProcessing={false}
            isStopping={false}
            enableStreaming={false}
            showConfidence={true}
            disableAutoScroll={disableAutoScroll}
            hasMore={hasMore}
            isLoadingMore={isLoadingMore}
            totalCount={totalCount}
            loadedCount={loadedCount}
            onLoadMore={onLoadMore}
          />
        )}
      </div>

      {/* Custom prompt input at bottom of transcript section */}
      {!isRecording && convertedSegments.length > 0 && (
        <div className="p-1 border-t border-gray-200">
          <textarea
            placeholder="Add context for AI summary. For example people involved, meeting overview, objective etc..."
            className="w-full px-3 py-2 border border-gray-200 rounded-md text-sm focus:outline-none focus:ring-1 focus:ring-blue-500 focus:border-blue-500 bg-white shadow-sm min-h-[80px] resize-y"
            value={customPrompt}
            onChange={(e) => onPromptChange(e.target.value)}
          />
        </div>
      )}
    </div>
  );
}
