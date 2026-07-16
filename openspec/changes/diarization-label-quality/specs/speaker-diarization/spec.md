## ADDED Requirements

### Requirement: Diarization segment granularity resolves speaker turns within Whisper segments

Whisper groups transcript segments by sentence/VAD, not by speaker; on multi-speaker meetings these segments routinely span 15–30s and contain two or more speakers. The diarization output SHALL be granular enough that a speaker turn occurring inside a single Whisper transcript segment produces a diarization segment boundary at or near the turn, so that per-word alignment can attribute the words on each side of the turn to the correct speakers rather than collapsing the whole segment to one speaker.

A turn of approximately 2 seconds or longer SHALL be resolved into its own speaker segment. Turns shorter than this MAY be absorbed into the dominant surrounding speaker (recovered instead by token-level word alignment).

#### Scenario: Sub-turn interjection is isolated, not swallowed

- **GIVEN** a Whisper transcript segment from 46:58 to 47:21 containing a 2s Ricardo interjection at 46:58–47:00 followed by Cynthia's speech
- **AND** the production diarization previously labeled the entire 46:58–47:30 run as Cynthia
- **WHEN** diarization runs with sub-turn granularity
- **THEN** the diarization output contains a speaker segment boundary near 47:00 separating Ricardo (≈46:50–47:00) from Cynthia (≈47:00 onward), so the interjection's words are attributed to Ricardo

#### Scenario: Back-and-forth between two speakers is not collapsed to one

- **GIVEN** a region where two speakers alternate in 4–8s turns across a 30s window
- **WHEN** diarization runs with sub-turn granularity
- **THEN** the output preserves the alternation as multiple segments rather than merging the window into a single speaker's run

#### Scenario: Single-speaker meeting is not fragmented

- **GIVEN** a meeting with exactly one speaker
- **WHEN** diarization runs with sub-turn granularity
- **THEN** the output is a single speaker (no spurious second cluster introduced by the finer chunking)

### Requirement: Short chunks are not attributed to temporally-absent speakers

A diarization chunk whose duration is below a minimum presence threshold SHALL NOT retain a speaker label that has no other temporal support in the surrounding neighborhood. Such a chunk SHALL be relabeled to the temporally-dominant local speaker. This prevents short, vowel-dominated embeddings from being globally assigned to a speaker who has not yet appeared (or has long since left) the meeting.

A chunk that is short but lies between two genuinely different speakers (a real interjection) is a legitimate turn and SHALL be preserved — only chunks whose assigned label is a temporal orphan are relabeled.

#### Scenario: Opening utterance is not attributed to a speaker who has not joined

- **GIVEN** a meeting where Speaker 2 (Ricardo) first appears at 17:37
- **AND** a 1.4s chunk at 0:01 ("Hello") whose raw embedding is globally nearest to Ricardo's centroid
- **WHEN** the temporal-presence constraint is applied
- **THEN** the 0:01 chunk is relabeled to a speaker present at the start of the meeting (not Ricardo), because Ricardo has no temporal support near 0:01

#### Scenario: Genuine short interjection between two speakers is preserved

- **GIVEN** a 2s chunk labeled Ricardo, sandwiched between a Cynthia segment (left) and a Carlos segment (right), where both neighbors differ from Ricardo
- **WHEN** the temporal-presence constraint is applied
- **THEN** the chunk retains the Ricardo label, because a short run between two different speakers is a real interjection, not a temporal orphan
