import { useCallback, useEffect, useState } from "react";
import { labelSpeaker, listSpeakers } from "@/services/speakerService";

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

    const handleSpeakerSubmit = useCallback(async (clusterLabel: string, name: string) => {
        if (!meetingId) return;
        try {
            await labelSpeaker(meetingId, clusterLabel, name);
            setEditingSegmentId(null);
            await onSpeakersChanged?.();
        } catch (err) {
            console.error("Failed to rename speaker:", err);
            setEditingSegmentId(null);
        }
    }, [meetingId, onSpeakersChanged]);

    return { editingSegmentId, setEditingSegmentId, knownSpeakers, handleSpeakerSubmit };
}
