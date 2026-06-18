"use client";

import { useState, useCallback } from 'react';
import { Button } from '@/components/ui/button';
import { ButtonGroup } from '@/components/ui/button-group';
import { Copy, FolderOpen, RefreshCw, Users } from 'lucide-react';
import Analytics from '@/lib/analytics';
import { RetranscribeDialog } from './RetranscribeDialog';
import { useConfig } from '@/contexts/ConfigContext';
import { resetSpeakerLabels } from '@/services/speakerService';
import { MeetingMaxSpeakersControl } from './MeetingMaxSpeakersControl';
import { toast } from 'sonner';


interface TranscriptButtonGroupProps {
  transcriptCount: number;
  onCopyTranscript: () => void;
  onOpenMeetingFolder: () => Promise<void>;
  meetingId?: string;
  meetingFolderPath?: string | null;
  onRefetchTranscripts?: () => Promise<void>;
}


export function TranscriptButtonGroup({
  transcriptCount,
  onCopyTranscript,
  onOpenMeetingFolder,
  meetingId,
  meetingFolderPath,
  onRefetchTranscripts,
}: TranscriptButtonGroupProps) {
  const { betaFeatures } = useConfig();
  const [showRetranscribeDialog, setShowRetranscribeDialog] = useState(false);
  const [isRediarizing, setIsRediarizing] = useState(false);

  const handleRediarize = useCallback(async () => {
    if (!meetingId) return;
    setIsRediarizing(true);
    try {
      const { listen } = await import('@tauri-apps/api/event');
      const unlisten = await listen<{ meeting_id: string; speaker_count: number; segments_labeled: number }>(
        'diarization-complete',
        async (event) => {
          if (event.payload.meeting_id === meetingId) {
            unlisten();
            if (onRefetchTranscripts) await onRefetchTranscripts();
            setIsRediarizing(false);
            toast.success(`Detected ${event.payload.speaker_count} speaker${event.payload.speaker_count !== 1 ? 's' : ''}`);
          }
        }
      );

      await resetSpeakerLabels(meetingId);
    } catch (e) {
      console.error('Re-diarization failed:', e);
      toast.error('Re-diarization failed', {
        description: e instanceof Error ? e.message : String(e),
      });
      setIsRediarizing(false);
    }
  }, [meetingId, onRefetchTranscripts]);

  const handleRetranscribeComplete = useCallback(async () => {
    // Refetch transcripts to show the updated data
    if (onRefetchTranscripts) {
      await onRefetchTranscripts();
    }
  }, [onRefetchTranscripts]);

  return (
    <div className="flex items-center justify-center w-full gap-2 overflow-x-auto">
      <ButtonGroup className="w-full flex-wrap justify-center">
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            Analytics.trackButtonClick('copy_transcript', 'meeting_details');
            onCopyTranscript();
          }}
          disabled={transcriptCount === 0}
          title={transcriptCount === 0 ? 'No transcript available' : 'Copy Transcript'}
        >
          <Copy />
          <span className="hidden lg:inline">Copy</span>
        </Button>

        <Button
          size="sm"
          variant="outline"
          className="xl:px-4"
          onClick={() => {
            Analytics.trackButtonClick('open_recording_folder', 'meeting_details');
            onOpenMeetingFolder();
          }}
          title="Open Recording Folder"
        >
          <FolderOpen className="xl:mr-2" size={18} />
          <span className="hidden lg:inline">Recording</span>
        </Button>

        {betaFeatures.importAndRetranscribe && meetingId && meetingFolderPath && (
          <Button
            size="sm"
            variant="outline"
            className="bg-gradient-to-r from-blue-50 to-purple-50 hover:from-blue-100 hover:to-purple-100 border-blue-200 xl:px-4"
            onClick={() => {
              Analytics.trackButtonClick('enhance_transcript', 'meeting_details');
              setShowRetranscribeDialog(true);
            }}
            title="Retranscribe to enhance your recorded audio"
          >
            <RefreshCw className="xl:mr-2" size={18} />
            <span className="hidden lg:inline">Enhance</span>
          </Button>
        )}

        {meetingId && transcriptCount > 0 && (
          <Button
            size="sm"
            variant="outline"
            className="xl:px-4"
            onClick={handleRediarize}
            disabled={isRediarizing}
            title="Re-run speaker detection on this meeting"
          >
            <Users className="xl:mr-2" size={18} />
            <span className="hidden lg:inline">{isRediarizing ? 'Analyzing…' : 'Speakers'}</span>
          </Button>
        )}
      </ButtonGroup>

      {meetingId && transcriptCount > 0 && (
        <MeetingMaxSpeakersControl meetingId={meetingId} />
      )}

      {betaFeatures.importAndRetranscribe && meetingId && meetingFolderPath && (
        <RetranscribeDialog
          open={showRetranscribeDialog}
          onOpenChange={setShowRetranscribeDialog}
          meetingId={meetingId}
          meetingFolderPath={meetingFolderPath}
          onComplete={handleRetranscribeComplete}
        />
      )}
    </div>
  );
}
