import { describe, it, expect } from 'vitest';
import { buildSpeakerIndexMap } from '@/components/VirtualizedTranscriptView';

// Speaker-split persistence: when a single source transcript row is replaced
// by N rows (one per diarized speaker), the renderer must surface every split
// as a distinct entry. React reconciliation keys TranscriptSegment by
// `segment.id` (not position), so split rows with fresh UUID ids never
// collapse. The pure logic that could break is the speaker → badge-index map:
// it must assign a distinct index to each distinct speaker so the two halves
// of a split row carry different badge colors. This test exercises the real
// extracted helper against a one→N split fixture.
describe('buildSpeakerIndexMap — one source row → N split rows', () => {
  it('assigns distinct indices to the two speakers of a split source row', () => {
    // Source row "src-1" was replaced by two fine rows with distinct ids.
    const splitRows = [
      { id: 'split-a', speaker: 'Speaker 0', text: 'first half' },
      { id: 'split-b', speaker: 'Speaker 1', text: 'second half' },
    ];
    const map = buildSpeakerIndexMap(splitRows);
    expect(map.size).toBe(2);
    expect(map.get('Speaker 0')).toBe(0);
    expect(map.get('Speaker 1')).toBe(1);
    // Distinct ids → both rows survive React keying (no positional collapse).
    expect(new Set(splitRows.map((r) => r.id)).size).toBe(splitRows.length);
  });

  it('handles variable N: a 3-way split yields three distinct indices', () => {
    const splitRows = [
      { id: 'a', speaker: 'Speaker 0', text: 'one' },
      { id: 'b', speaker: 'Speaker 1', text: 'two' },
      { id: 'c', speaker: 'Speaker 2', text: 'three' },
    ];
    const map = buildSpeakerIndexMap(splitRows);
    expect(map.size).toBe(3);
    const indices = [...map.values()];
    expect(new Set(indices).size).toBe(3);
  });

  it('reuses a speaker index when a speaker recurs across split rows (stability)', () => {
    // Speaker 0 appears in two split rows from different source rows — both
    // must share index 0 so the badge color is consistent across the meeting.
    const rows = [
      { id: 's1-a', speaker: 'Speaker 0', text: 'x' },
      { id: 's1-b', speaker: 'Speaker 1', text: 'y' },
      { id: 's2-a', speaker: 'Speaker 0', text: 'z' },
    ];
    const map = buildSpeakerIndexMap(rows);
    expect(map.get('Speaker 0')).toBe(0);
    expect(map.get('Speaker 1')).toBe(1);
    expect(map.size).toBe(2);
  });

  it('treats an Unknown-Speaker tail as its own distinct badge', () => {
    // Proportional alignment emits "Unknown Speaker" for the no-overlap tail;
    // it must get its own index, distinct from any diarized speaker.
    const rows = [
      { id: 'o-1', speaker: 'Speaker 0', text: 'covered' },
      { id: 'o-2', speaker: 'Unknown Speaker', text: 'tail' },
    ];
    const map = buildSpeakerIndexMap(rows);
    expect(map.size).toBe(2);
    expect(map.get('Speaker 0')).not.toBe(map.get('Unknown Speaker'));
  });

  it('preserves distinct ids across a large split (no id collision)', () => {
    const n = 50;
    const rows = Array.from({ length: n }, (_, i) => ({
      id: `split-${i}`,
      speaker: `Speaker ${i}`,
      text: `seg-${i}`,
    }));
    const ids = rows.map((r) => r.id);
    expect(new Set(ids).size).toBe(n);
    const map = buildSpeakerIndexMap(rows);
    expect(map.size).toBe(n);
  });
});
