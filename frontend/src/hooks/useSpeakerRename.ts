import { useCallback, useEffect, useState } from "react";
import { labelSpeaker, listSpeakers, revertSpeakerLabel, setSegmentSpeaker } from "@/services/speakerService";

export function useSpeakerRename(
    meetingId: string | undefined,
    onSpeakersChanged: (() => Promise<void>) | undefined,
) {
    const [editingSegmentId, setEditingSegmentId] = useState<string | null>(null);
    const [knownSpeakers, setKnownSpeakers] = useState<string[]>([]);

    useEffect(() => {
        listSpeakers().then(speakers => {
            setKnownSpeakers(speakers.map(s => s.name).filter(n => !n.startsWith("Speaker ")));
        }).catch(() => {});
    }, []);

    const handleSpeakerSubmit = useCallback(async (
        transcriptId: string,
        clusterLabel: string,
        name: string,
        scope: 'cluster' | 'segment',
    ) => {
        if (!meetingId) return;
        try {
            if (scope === 'segment') {
                await setSegmentSpeaker(transcriptId, name);
            } else {
                await labelSpeaker(meetingId, clusterLabel, name);
            }
            setEditingSegmentId(null);
            await onSpeakersChanged?.();
        } catch (err) {
            console.error("Failed to rename speaker:", err);
            setEditingSegmentId(null);
        }
    }, [meetingId, onSpeakersChanged]);

    const handleSpeakerRevert = useCallback(async (speakerLabel: string) => {
        if (!meetingId) return;
        try {
            await revertSpeakerLabel(meetingId, speakerLabel);
            await onSpeakersChanged?.();
        } catch (err) {
            console.error("Failed to revert speaker:", err);
        }
    }, [meetingId, onSpeakersChanged]);

    return { editingSegmentId, setEditingSegmentId, knownSpeakers, handleSpeakerSubmit, handleSpeakerRevert };
}
