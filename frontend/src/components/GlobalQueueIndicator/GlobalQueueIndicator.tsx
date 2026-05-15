/**
 * GlobalQueueIndicator
 *
 * App-shell widget showing overall queue status with Pause/Resume controls.
 * Visible when there are active (non-done, non-failed) jobs in the queue.
 */
import React, { useState } from 'react';
import { useQueueSnapshot } from '@/hooks/useQueueJobStatus';
import { pauseAllBackgroundWork, resumeAllBackgroundWork } from '@/services/queueService';
import { Pause, Play, Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { cn } from '@/lib/utils';

interface GlobalQueueIndicatorProps {
  className?: string;
}

export function GlobalQueueIndicator({ className }: GlobalQueueIndicatorProps) {
  const snapshot = useQueueSnapshot();
  const [isToggling, setIsToggling] = useState(false);

  const activeJobs = snapshot.jobs.filter(j => j.status !== 'Done' && j.status !== 'Failed');
  const pausedJobs = activeJobs.filter(j => j.status === 'Paused');
  const inProgressJobs = activeJobs.filter(j => j.status === 'InProgress');
  const allPaused = activeJobs.length > 0 && pausedJobs.length === activeJobs.length;

  if (activeJobs.length === 0) return null;

  const statusLabel = allPaused
    ? `${activeJobs.length} queued (paused)`
    : inProgressJobs.length > 0
    ? `${activeJobs.length} queued (running)`
    : `${activeJobs.length} queued`;

  const handleToggle = async () => {
    setIsToggling(true);
    try {
      if (allPaused) {
        await resumeAllBackgroundWork();
      } else {
        await pauseAllBackgroundWork();
      }
    } catch (err) {
      console.error('Failed to toggle queue:', err);
    } finally {
      setIsToggling(false);
    }
  };

  return (
    <div
      className={cn(
        'flex items-center gap-2 rounded-lg border bg-card px-3 py-1.5 text-sm shadow-sm',
        className,
      )}
    >
      {inProgressJobs.length > 0 && (
        <Loader2 className="w-3.5 h-3.5 animate-spin text-blue-500 shrink-0" />
      )}
      <span className="text-muted-foreground">{statusLabel}</span>
      <Button
        size="sm"
        variant="ghost"
        className="h-6 w-6 p-0"
        onClick={handleToggle}
        disabled={isToggling}
        title={allPaused ? 'Resume all background work' : 'Pause all background work'}
        aria-label={allPaused ? 'Resume' : 'Pause'}
      >
        {allPaused
          ? <Play className="w-3 h-3" />
          : <Pause className="w-3 h-3" />
        }
      </Button>
    </div>
  );
}
