'use client';

import React, { createContext, useContext, useState, useEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

const PARAKEET_MODEL = 'parakeet-tdt-0.6b-v3-int8';

interface OnboardingStatus {
  version: string;
  completed: boolean;
  current_step: number;
  model_status: {
    parakeet: string;
    summary: string;
  };
  last_updated: string;
}

interface OnboardingContextType {
  currentStep: number;
  parakeetDownloaded: boolean;
  parakeetProgress: number;
  summaryModelDownloaded: boolean;
  summaryModelProgress: number;
  selectedSummaryModel: string;
  databaseExists: boolean;
  goToStep: (step: number) => void;
  goNext: () => void;
  goPrevious: () => void;
  setParakeetDownloaded: (value: boolean) => void;
  setSummaryModelDownloaded: (value: boolean) => void;
  setSelectedSummaryModel: (value: string) => void;
  setDatabaseExists: (value: boolean) => void;
  completeOnboarding: () => Promise<void>;
}

const OnboardingContext = createContext<OnboardingContextType | undefined>(undefined);

export function OnboardingProvider({ children }: { children: React.ReactNode }) {
  const [currentStep, setCurrentStep] = useState(1);
  const [completed, setCompleted] = useState(false);
  const [parakeetDownloaded, setParakeetDownloaded] = useState(false);
  const [parakeetProgress, setParakeetProgress] = useState(0);
  const [summaryModelDownloaded, setSummaryModelDownloaded] = useState(false);
  const [summaryModelProgress, setSummaryModelProgress] = useState(0);
  const [selectedSummaryModel, setSelectedSummaryModel] = useState<string>('gemma3:1b');
  const [databaseExists, setDatabaseExists] = useState(false);

  const saveTimeoutRef = useRef<NodeJS.Timeout>();

  // Load status on mount
  useEffect(() => {
    loadOnboardingStatus();
    checkDatabaseStatus();
  }, []);

  // Auto-save on state change (debounced)
  useEffect(() => {
    if (saveTimeoutRef.current) clearTimeout(saveTimeoutRef.current);

    // Don't auto-save if completed (to avoid overwriting completion status)
    if (completed) return;

    saveTimeoutRef.current = setTimeout(() => {
      saveOnboardingStatus();
    }, 1000);

    return () => {
      if (saveTimeoutRef.current) clearTimeout(saveTimeoutRef.current);
    };
  }, [currentStep, parakeetDownloaded, summaryModelDownloaded, completed]);

  // Listen to Parakeet download progress
  useEffect(() => {
    const unlisten = listen<{ modelName: string; progress: number }>(
      'parakeet-model-download-progress',
      (event) => {
        const { modelName, progress } = event.payload;
        if (modelName === PARAKEET_MODEL) {
          setParakeetProgress(progress);
        }
      }
    );

    const unlistenComplete = listen<{ modelName: string }>(
      'parakeet-model-download-complete',
      (event) => {
        const { modelName } = event.payload;
        if (modelName === PARAKEET_MODEL) {
          setParakeetDownloaded(true);
          setParakeetProgress(100);
        }
      }
    );

    const unlistenError = listen<{ modelName: string; error: string }>(
      'parakeet-model-download-error',
      (event) => {
        const { modelName } = event.payload;
        if (modelName === PARAKEET_MODEL) {
          console.error('Parakeet download error:', event.payload.error);
        }
      }
    );

    return () => {
      unlisten.then(fn => fn());
      unlistenComplete.then(fn => fn());
      unlistenError.then(fn => fn());
    };
  }, []);

  // Listen to summary model (Built-in AI) download progress
  useEffect(() => {
    const unlisten = listen<{ model: string; progress: number; status: string }>(
      'builtin-ai-download-progress',
      (event) => {
        const { model, progress, status } = event.payload;
        // Check if this is the selected summary model (gemma3:1b or mistral:7b)
        if (model === selectedSummaryModel || model === 'gemma3:1b' || model === 'mistral:7b') {
          setSummaryModelProgress(progress);
          if (status === 'completed' || progress >= 100) {
            setSummaryModelDownloaded(true);
          }
        }
      }
    );

    return () => {
      unlisten.then(fn => fn());
    };
  }, [selectedSummaryModel]);

  const checkDatabaseStatus = async () => {
    try {
      const isFirstLaunch = await invoke<boolean>('check_first_launch');
      setDatabaseExists(!isFirstLaunch);
      console.log('[OnboardingContext] Database exists:', !isFirstLaunch);
    } catch (error) {
      console.error('[OnboardingContext] Failed to check database status:', error);
      setDatabaseExists(false);
    }
  };

  const loadOnboardingStatus = async () => {
    try {
      const status = await invoke<OnboardingStatus | null>('get_onboarding_status');
      if (status) {
        setCurrentStep(status.current_step);
        setCompleted(status.completed);
        setParakeetDownloaded(status.model_status.parakeet === 'downloaded');
        setSummaryModelDownloaded(status.model_status.summary === 'downloaded');
        console.log('[OnboardingContext] Loaded status:', status);
      }
    } catch (error) {
      console.error('[OnboardingContext] Failed to load onboarding status:', error);
    }
  };

  const saveOnboardingStatus = async () => {
    try {
      await invoke('save_onboarding_status_cmd', {
        status: {
          version: '1.0',
          completed: completed,
          current_step: currentStep,
          model_status: {
            parakeet: parakeetDownloaded ? 'downloaded' : 'not_downloaded',
            summary: summaryModelDownloaded ? 'downloaded' : 'not_downloaded',
          },
          last_updated: new Date().toISOString(),
        },
      });
    } catch (error) {
      console.error('[OnboardingContext] Failed to save onboarding status:', error);
    }
  };

  const completeOnboarding = async () => {
    try {
      // Pass the selected summary model to backend to save in database
      await invoke('complete_onboarding', {
        summaryModel: selectedSummaryModel,
      });
      setCompleted(true);
      console.log('[OnboardingContext] Onboarding completed with model:', selectedSummaryModel);
    } catch (error) {
      console.error('[OnboardingContext] Failed to complete onboarding:', error);
      throw error; // Re-throw so CompletionStep can handle it
    }
  };

  const goToStep = useCallback((step: number) => {
    setCurrentStep(Math.max(1, Math.min(step, 5)));
  }, []);

  const goNext = useCallback(() => {
    setCurrentStep(prev => {
      const next = prev + 1;
      // Don't go past step 5
      return Math.min(next, 5);
    });
  }, []);

  const goPrevious = useCallback(() => {
    setCurrentStep(prev => {
      const previous = prev - 1;
      // Don't go below step 1
      return Math.max(previous, 1);
    });
  }, []);

  return (
    <OnboardingContext.Provider
      value={{
        currentStep,
        parakeetDownloaded,
        parakeetProgress,
        summaryModelDownloaded,
        summaryModelProgress,
        selectedSummaryModel,
        databaseExists,
        goToStep,
        goNext,
        goPrevious,
        setParakeetDownloaded,
        setSummaryModelDownloaded,
        setSelectedSummaryModel,
        setDatabaseExists,
        completeOnboarding,
      }}
    >
      {children}
    </OnboardingContext.Provider>
  );
}

export function useOnboarding() {
  const context = useContext(OnboardingContext);
  if (!context) {
    throw new Error('useOnboarding must be used within OnboardingProvider');
  }
  return context;
}
