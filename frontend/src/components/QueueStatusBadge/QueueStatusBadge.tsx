/**
 * QueueStatusBadge
 *
 * Per-meeting status pill driven by transcription-queue-changed events.
 * Render states: Transcribing | Summarising | Queued | Paused | Done | Failed | (hidden)
 */
import React, { useState } from 'react';
import { toast } from 'sonner';
import { QueueJob } from '@/services/queueService';
import { queueJobLabel, shouldShowRunNow, useSchedulerSettings } from '@/hooks/useQueueJobStatus';
import { cn } from '@/lib/utils';
import { cancelQueuedJob } from '@/services/queueService';
import { runTranscriptionJobNow } from '@/services/schedulerSettingsService';
import { Play, X } from 'lucide-react';

interface QueueStatusBadgeProps {
  job: QueueJob | undefined;
  /** Show a cancel (×) button — task 10.4 */
  showCancel?: boolean;
  onCancelled?: () => void;
  className?: string;
}

function badgeVariant(job: QueueJob): string {
  switch (job.status) {
    case 'InProgress': return 'bg-blue-100 text-blue-800 border-blue-200';
    case 'Pending':    return 'bg-yellow-100 text-yellow-800 border-yellow-200';
    case 'Paused':     return 'bg-orange-100 text-orange-800 border-orange-200';
    case 'Done':       return 'bg-green-100 text-green-800 border-green-200';
    case 'Failed':     return 'bg-red-100 text-red-800 border-red-200';
    default:           return 'bg-gray-100 text-gray-700 border-gray-200';
  }
}

export function QueueStatusBadge({ job, showCancel = false, onCancelled, className }: QueueStatusBadgeProps) {
  const settings = useSchedulerSettings();
  const [isStarting, setIsStarting] = useState(false);

  if (!job || job.status === 'Done') return null;

  const handleCancel = async (e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await cancelQueuedJob(job.meeting_id);
      toast.success('Transcription cancelled');
      onCancelled?.();
    } catch (err) {
      console.error('Failed to cancel queue job:', err);
      toast.error('Failed to cancel transcription');
    }
  };

  const handleRunNow = async (e: React.MouseEvent) => {
    e.stopPropagation();
    setIsStarting(true);
    try {
      const started = await runTranscriptionJobNow(job.meeting_id);
      if (!started) {
        toast.error('Cannot start — system is busy');
      }
    } catch (err) {
      toast.error('Failed to start transcription');
    } finally {
      setIsStarting(false);
    }
  };

  return (
    <span
      className={cn(
        'inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs font-medium w-fit',
        badgeVariant(job),
        className,
      )}
    >
      {job.status === 'InProgress' && (
        <span className="w-1.5 h-1.5 rounded-full bg-current animate-pulse flex-shrink-0" />
      )}
      {queueJobLabel(job, settings)}
      {showCancel && job.status !== 'Failed' && (
        <button
          onClick={handleCancel}
          className="ml-auto rounded-full hover:bg-black/10 p-1 flex-shrink-0"
          title="Cancel transcription"
          aria-label="Cancel transcription"
        >
          <X className="w-3 h-3" />
        </button>
      )}
      {shouldShowRunNow(job, settings) && (
        <button
          onClick={handleRunNow}
          disabled={isStarting}
          className="ml-1 rounded-full hover:bg-black/10 p-1 flex-shrink-0 disabled:opacity-50"
          title="Start transcription now"
          aria-label="Run now"
        >
          <Play className="w-3 h-3" />
        </button>
      )}
    </span>
  );
}
