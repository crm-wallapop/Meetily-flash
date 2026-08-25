# Tasks: Speaker Button Timeout & Error Recovery

## Completed

- [x] Identify root cause: `await resetSpeakerLabels()` blocks UI thread indefinitely when backend dies
- [x] Fix frontend: replace `await` with `Promise.race` (event vs command vs 5-min timeout)
- [x] Add `finally` block to guarantee `setIsRediarizing(false)` on all paths
- [x] Add error handling for timeout vs explicit rejection
- [x] Verify button disabled state via `disabled={isRediarizing}` prevents double-clicks
- [x] Confirm backend `diarization_lock` prevents parallel jobs for same meeting

## In Progress

- [ ] Manual smoke test: start dev server, click button, kill server, verify timeout recovers UI
- [ ] Verify no regression: normal diarization completion still shows success toast + refetches

## Blocked / Needs Decision

- [ ] Backend timeout configuration: should `reset_speaker_labels` command have its own timeout (independent of Tauri's 60s default)?
  - Currently: command runs in `tokio::spawn` + `.await` on JoinHandle — if JoinHandle hangs, command hangs
  - Could add: `tokio::time::timeout` around the JoinHandle await
  - Trade-off: shorter timeout catches dead backends faster but risks killing legitimate long diarizations

## Out of Scope (Future)

- [ ] Backend cancellation API for in-progress diarization
- [ ] Progress indicator (percentage, elapsed time) during diarization
- [ ] Retry button in error toast (instead of re-clicking Speakers button)

## Notes

The implementation is complete in `TranscriptButtonGroup.tsx`. The only remaining item is manual verification by running `pnpm tauri dev` and testing the timeout scenario.