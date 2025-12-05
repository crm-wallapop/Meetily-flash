import React, { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Sparkles, Mic, AlertCircle, Lock, Volume2, Download } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { OnboardingContainer } from '../OnboardingContainer';
import { useOnboarding } from '@/contexts/OnboardingContext';

export function SetupOverviewStep() {
  const { goNext, setDatabaseExists } = useOnboarding();
  const [dbError, setDbError] = useState<string | null>(null);
  const [isInitializing, setIsInitializing] = useState(true);

  // Silently initialize database in background on mount
  useEffect(() => {
    initializeDatabaseInBackground();
  }, []);

  const initializeDatabaseInBackground = async () => {
    try {
      console.log('[SetupOverviewStep] Starting background database initialization');

      // Check if database already exists (not first launch)
      const isFirstLaunch = await invoke<boolean>('check_first_launch');

      if (!isFirstLaunch) {
        // Database exists, just mark as ready
        console.log('[SetupOverviewStep] Database exists, skipping initialization');
        setDatabaseExists(true);
        setIsInitializing(false);
        return;
      }

      // First launch - attempt auto-detection and import
      await performAutoDetection();
      setIsInitializing(false);
    } catch (error) {
      console.error('[SetupOverviewStep] Database initialization failed:', error);
      setDbError(error instanceof Error ? error.message : 'Database initialization failed');
      setIsInitializing(false);
    }
  };

  const performAutoDetection = async () => {
    // Check Homebrew (macOS only)
    if (navigator.platform.toLowerCase().includes('mac')) {
      const homebrewDbPath = '/usr/local/var/meetily/meeting_minutes.db';
      const homebrewCheck = await invoke<{ exists: boolean; size: number } | null>(
        'check_homebrew_database',
        { path: homebrewDbPath }
      );

      if (homebrewCheck?.exists) {
        console.log('[SetupOverviewStep] Found Homebrew database, importing');
        await invoke('import_and_initialize_database', {
          legacyDbPath: homebrewDbPath,
        });
        setDatabaseExists(true);
        return;
      }
    }

    // Check default legacy database location
    const legacyPath = await invoke<string | null>('check_default_legacy_database');

    if (legacyPath) {
      console.log('[SetupOverviewStep] Found legacy database, importing');
      await invoke('import_and_initialize_database', {
        legacyDbPath: legacyPath,
      });
      setDatabaseExists(true);
      return;
    }

    // No legacy database found - initialize fresh
    console.log('[SetupOverviewStep] No legacy database found, initializing fresh');
    await invoke('initialize_fresh_database');
    setDatabaseExists(true);
  };

  const handleContinue = () => {
    if (dbError) {
      // Show error, don't allow to continue
      return;
    }
    goNext();
  };

  const handleRetry = () => {
    setDbError(null);
    setIsInitializing(true);
    initializeDatabaseInBackground();
  };

  const steps = [
    {
      number: 1,
      title: 'Download Transcription Model',
      description: 'Parakeet v3 (~670 MB)',
      icon: Mic
    },
    {
      number: 2,
      title: 'Download Summarization Model',
      description: 'Gemma 3',
      icon: Sparkles
    },
  ];

  return (
    <OnboardingContainer
      title="Setup Overview"
      description={
        isInitializing
          ? 'Preparing your workspace...'
          : "Meeetily requires that you download the Trancription & Summarization AI models for the software to work."
      }
      step={1}
      totalSteps={4}
      stepOffset={1}
    >
      <div className="flex flex-col items-center space-y-10">
        {/* Database Error Banner */}
        {dbError && (
          <div className="w-full max-w-md bg-red-50 border border-red-200 rounded-lg p-4">
            <div className="flex items-start gap-3">
              <AlertCircle className="w-5 h-5 text-red-600 flex-shrink-0 mt-0.5" />
              <div className="flex-1">
                <h3 className="font-medium text-red-800 mb-1">Database Error</h3>
                <p className="text-sm text-red-700">{dbError}</p>
                <Button
                  onClick={handleRetry}
                  variant="outline"
                  size="sm"
                  className="mt-3 border-red-300 text-red-700 hover:bg-red-100"
                >
                  Retry
                </Button>
              </div>
            </div>
          </div>
        )}

        {/* Steps List */}
        <div className="w-full max-w-md space-y-4">
          {steps.map((step) => {
            const Icon = step.icon;
            return (
              <div
                key={step.number}
                className="flex items-center gap-4 bg-white rounded-lg border border-gray-200 p-4"
              >
                <div className="flex-shrink-0">
                  <div className="w-10 h-10 rounded-full bg-gray-100 flex items-center justify-center">
                    <Icon className="w-5 h-5 text-gray-600" />
                  </div>
                </div>
                <div className="flex-1">
                  <h3 className="font-medium text-gray-900">Step {step.number}: {step.title}</h3>
                  {step.description && (
                    <p className="text-sm text-gray-500">{step.description}</p>
                  )}
                </div>
              </div>
            );
          })}
        </div>

        {/* CTA Section */}
        <div className="w-full max-w-xs space-y-4">
          <Button
            onClick={handleContinue}
            disabled={isInitializing || !!dbError}
            className="w-full h-11 bg-gray-900 hover:bg-gray-800 text-white disabled:bg-gray-400 disabled:cursor-not-allowed"
          >
            {isInitializing ? 'Initializing...' : "Let's Go"}
          </Button>
          <div className="text-center">
            <a
              href="https://github.com/Zackriya-Solutions/meeting-minutes"
              target="_blank"
              rel="noopener noreferrer"
              className="text-xs text-gray-600 hover:underline"
            >
              Report issues on GitHub
            </a>
          </div>
        </div>
      </div>
    </OnboardingContainer>
  );
}
