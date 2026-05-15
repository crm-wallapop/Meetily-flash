/**
 * QueueStatusBadge
 *
 * Per-meeting status pill driven by transcription-queue-changed events.
 * Render states: Transcribing | Summarising | Queued | Paused | Done | Failed | (hidden)
 */
import React from 'react';
import { QueueJob } from '@/services/queueService';
import { queueJobLabel } from '@/hooks/useQueueJobStatus';
import { cn } from '@/lib/utils';
import { cancelQueuedJob } from '@/services/queueService';
import { X } from 'lucide-react';

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
  if (!job || job.status === 'Done') return null;

  const handleCancel = async (e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await cancelQueuedJob(job.meeting_id);
      onCancelled?.();
    } catch (err) {
      console.error('Failed to cancel queue job:', err);
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
      {queueJobLabel(job)}
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
    </span>
  );
}
