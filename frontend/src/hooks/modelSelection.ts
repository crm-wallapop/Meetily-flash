// Enhance-dialog default-model selection.
//
// Pure module (no Tauri imports) so the policy is unit-testable in vitest.
// See pickDefaultModel for the fallback-notice contract.

import type { ModelOption } from './useTranscriptionModels';

/// Quality-first preference order for Whisper models. Used when the
/// configured model isn't available locally, replacing the previous
/// "first in discovery order" fallback, which could silently select a tiny
/// model where large-v3 quality was expected (runtime and quality vary ~10x
/// across this list).
const QUALITY_ORDER: string[] = [
  'large-v3',
  'large-v3-turbo',
  'large-v3-q5_0',
  'large-v3-turbo-q5_0',
  'medium',
  'medium-q5_0',
  'small',
  'base',
  'base-q5_1',
  'tiny',
  'tiny-q5_1',
];

export interface DefaultModelPick {
  /// `provider:name` key for the Select, or null when nothing is listed.
  key: string | null;
  /// Set when the user's configured Whisper model is not what got selected
  /// (it is unavailable locally). Rendered as a visible notice in the dialog
  /// so the fallback is never silent.
  fallbackNotice: string | null;
}

/// Pick the Enhance dialog's default model.
///
/// - configured Whisper model available  → it, no notice
/// - configured Whisper model missing    → best available by QUALITY_ORDER,
///   with a fallback notice naming both models
/// - configured provider is not Whisper  → best available, WITH an
///   engine-substitution notice: Parakeet emits no word timestamps, which
///   speaker alignment requires, so the divergence is always visible
/// - nothing listed                      → null key, no notice
export function pickDefaultModel(
  listed: ModelOption[],
  configuredProvider: string | undefined,
  configuredModel: string | undefined,
): DefaultModelPick {
  if (listed.length === 0) {
    return { key: null, fallbackNotice: null };
  }

  const byName = new Map(listed.map((mo) => [mo.name, mo]));
  const isWhisperConfig = configuredProvider === 'localWhisper' || configuredProvider === 'whisper';
  const configured =
    isWhisperConfig && configuredModel ? byName.get(configuredModel) : undefined;

  if (configured) {
    return { key: `${configured.provider}:${configured.name}`, fallbackNotice: null };
  }

  const best =
    QUALITY_ORDER.map((n) => byName.get(n)).find((mo) => mo !== undefined) ?? listed[0];

  const notice =
    isWhisperConfig && configuredModel
      ? `Configured model '${configuredModel}' is not available — using '${best.name}' instead.`
      : configuredProvider === 'parakeet'
        ? `Configured engine 'Parakeet' is not used by Enhance — it emits no word timestamps, which speaker alignment requires. Using Whisper instead.`
        : null;

  return { key: `${best.provider}:${best.name}`, fallbackNotice: notice };
}
