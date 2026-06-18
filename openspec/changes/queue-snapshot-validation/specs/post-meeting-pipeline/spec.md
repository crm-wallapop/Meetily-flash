## ADDED Requirements

### Requirement: Frontend tolerates malformed queue-state payloads without crashing

The frontend adapter SHALL normalize every `QueueSnapshot` payload received from `get_queue_state` and `transcription-queue-changed` events before it enters React state, coercing any missing or wrong-typed `jobs` to an empty array and any missing or wrong-typed `manual_pause_all` to `false`. No consumer of the queue snapshot SHALL reach a `.find` (or any array operation) on `jobs` without the adapter's guarantee that it is an array.

#### Scenario: Missing jobs array does not crash

- **WHEN** a `transcription-queue-changed` event or `get_queue_state` response arrives with a payload lacking a `jobs` field (e.g. `{ manual_pause_all: true }`)
- **THEN** the frontend stores `{ jobs: [], manual_pause_all: true }` in state and no `Cannot read properties of undefined (reading 'find')` error is thrown

#### Scenario: Non-array jobs does not crash

- **WHEN** a queue-state payload arrives with `jobs` set to a non-array value (e.g. a string or `null`)
- **THEN** the frontend coerces `jobs` to `[]` and continues rendering

#### Scenario: Missing manual_pause_all defaults to false

- **WHEN** a payload arrives without `manual_pause_all`
- **THEN** the frontend treats it as `false`

#### Scenario: Well-formed payload passes through unchanged

- **WHEN** a payload arrives with a valid `jobs` array and a boolean `manual_pause_all`
- **THEN** the normalized snapshot is structurally identical to the input (no fields dropped, no values coerced)

#### Scenario: Sidebar renders meeting items on a malformed payload

- **WHEN** the Meeting Notes sidebar expands to render meeting items AND the queue-state payload is malformed (missing `jobs`)
- **THEN** the meeting items render without a runtime error and the page emits no uncaught error
