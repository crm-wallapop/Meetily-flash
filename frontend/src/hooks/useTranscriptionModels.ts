import { useState, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { pickDefaultModel } from './modelSelection';

export interface RawModelInfo {
  name: string;
  size_mb: number;
  status: 'Available' | 'Missing' | { Downloading: { progress: number } } | { Error: string };
}

export interface ModelOption {
  provider: 'whisper' | 'parakeet';
  name: string;
  displayName: string;
  size_mb: number;
}

interface TranscriptModelConfig {
  provider?: string;
  model?: string;
}

/**
 * Custom hook for fetching and managing transcription models (Whisper and Parakeet).
 *
 * This hook centralizes the model fetching logic that was previously duplicated
 * in ImportAudioDialog and RetranscribeDialog components.
 *
 * @param transcriptModelConfig - User's saved model configuration from context
 * @returns Object containing available models, selected model key, loading state, and fetch function
 */
export function useTranscriptionModels(transcriptModelConfig: TranscriptModelConfig | undefined) {
  const [availableModels, setAvailableModels] = useState<ModelOption[]>([]);
  const [selectedModelKey, setSelectedModelKey] = useState<string>('');
  const [loadingModels, setLoadingModels] = useState(false);
  const [fallbackNotice, setFallbackNotice] = useState<string | null>(null);
  // Track whether the user has manually changed the model selection
  const userSelectedRef = useRef(false);

  // Wrap setSelectedModelKey to track user-initiated changes
  const setSelectedModelKeyWithTracking = useCallback((key: string) => {
    userSelectedRef.current = true;
    setSelectedModelKey(key);
  }, []);

  const fetchModels = useCallback(async () => {
    setLoadingModels(true);
    const allModels: ModelOption[] = [];

    // Fetch Whisper models
    try {
      const whisperModels = await invoke<RawModelInfo[]>('whisper_get_available_models');
      const availableWhisper = whisperModels
        .filter((m) => m.status === 'Available')
        .map((m) => ({
          provider: 'whisper' as const,
          name: m.name,
          displayName: `🏠 Whisper: ${m.name}`,
          size_mb: m.size_mb,
        }));
      allModels.push(...availableWhisper);
    } catch (err) {
      console.error('Failed to fetch Whisper models:', err);
    }

    // Fetch Parakeet models
    try {
      const parakeetModels = await invoke<RawModelInfo[]>('parakeet_get_available_models');
      const availableParakeet = parakeetModels
        .filter((m) => m.status === 'Available')
        .map((m) => ({
          provider: 'parakeet' as const,
          name: m.name,
          displayName: `⚡ Parakeet: ${m.name}`,
          size_mb: m.size_mb,
        }));
      allModels.push(...availableParakeet);
    } catch (err) {
      console.error('Failed to fetch Parakeet models:', err);
    }

    // Whisper-only listing (user decision 2026-08-27): Enhance is the
    // diarization boundary source, and Parakeet rows carry no token
    // timestamps — alignment then falls back to proportional word-count
    // slicing and cuts sentences mid-word at speaker changes. Whisper rows
    // carry token timestamps and split word-exactly via align_with_tokens.
    // Parakeet stays listed only when no Whisper model exists locally (a
    // dead dropdown is worse than a token-less fallback).
    const whisperModels = allModels.filter((m) => m.provider === 'whisper');
    const listed = whisperModels.length > 0 ? whisperModels : allModels;
    setAvailableModels(listed);

    // Default selection: configured Whisper model if available, else the
    // best available by quality order — never silently. A fallback from the
    // configured model surfaces as fallbackNotice in the dialog.
    const pick = pickDefaultModel(
      listed,
      transcriptModelConfig?.provider,
      transcriptModelConfig?.model,
    );
    if (!userSelectedRef.current && pick.key) {
      setSelectedModelKey(pick.key);
    }
    setFallbackNotice(pick.fallbackNotice);

    setLoadingModels(false);
  }, [transcriptModelConfig]);

  // Reset user selection tracking (call when dialog opens fresh)
  const resetSelection = useCallback(() => {
    userSelectedRef.current = false;
  }, []);

  return {
    availableModels,
    fallbackNotice,
    selectedModelKey,
    setSelectedModelKey: setSelectedModelKeyWithTracking,
    loadingModels,
    fetchModels,
    resetSelection,
  };
}
