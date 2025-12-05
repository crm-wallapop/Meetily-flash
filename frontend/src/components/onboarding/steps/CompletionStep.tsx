import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { CheckCircle2, AlertCircle, Loader2, XCircle } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { OnboardingContainer } from '../OnboardingContainer';
import { useOnboarding } from '@/contexts/OnboardingContext';

type VerificationStatus = 'checking' | 'ready' | 'missing';

interface ModelVerification {
  parakeet: VerificationStatus;
  summary: VerificationStatus;
}

export function CompletionStep({ isMac }: { isMac: boolean }) {
  const { completeOnboarding, goNext, goToStep, setParakeetDownloaded, setSummaryModelDownloaded } = useOnboarding();
  const [isCompleting, setIsCompleting] = useState(false);
  const [completionError, setCompletionError] = useState<string | null>(null);
  const [verification, setVerification] = useState<ModelVerification>({
    parakeet: 'checking',
    summary: 'checking',
  });

  // Verify models on mount
  useEffect(() => {
    verifyModels();
  }, []);

  const verifyModels = async () => {
    setVerification({ parakeet: 'checking', summary: 'checking' });

    // Verify Parakeet
    let parakeetStatus: VerificationStatus = 'missing';
    try {
      await invoke('parakeet_init');
      const exists = await invoke<boolean>('parakeet_has_available_models');
      parakeetStatus = exists ? 'ready' : 'missing';
      console.log('[CompletionStep] Parakeet verified:', parakeetStatus);
    } catch (error) {
      console.error('[CompletionStep] Failed to verify Parakeet:', error);
      parakeetStatus = 'missing';
    }

    // Verify Summary - check if ANY summary model is available
    let summaryStatus: VerificationStatus = 'missing';
    try {
      const availableModel = await invoke<string | null>('builtin_ai_get_available_summary_model');
      summaryStatus = availableModel ? 'ready' : 'missing';
      console.log('[CompletionStep] Summary verified:', summaryStatus, 'model:', availableModel);
    } catch (error) {
      console.error('[CompletionStep] Failed to verify Summary:', error);
      summaryStatus = 'missing';
    }

    setVerification({
      parakeet: parakeetStatus,
      summary: summaryStatus,
    });

    // If any model is missing, show error
    if (parakeetStatus === 'missing' || summaryStatus === 'missing') {
      setCompletionError(
        'Some models are missing. Please go back and complete the download steps.'
      );
    }
  };

  const handleDone = async () => {
    // Don't allow completion if models are missing
    if (verification.parakeet !== 'ready' || verification.summary !== 'ready') {
      setCompletionError('Cannot complete setup - required models are missing.');
      return;
    }

    if (isMac) {
      goNext();
      return;
    }

    setIsCompleting(true);
    setCompletionError(null);

    try {
      await completeOnboarding();
      // Force a reload to ensure the main app loads with the new state
      window.location.reload();
    } catch (error) {
      console.error('Failed to complete onboarding:', error);
      setCompletionError(
        error instanceof Error
          ? error.message
          : 'Failed to save configuration. Please try again.'
      );
      setIsCompleting(false);
    }
  };

  const handleGoBack = (step: number) => {
    // Update context to reflect missing status BEFORE navigating
    // This prevents the download step's sync effect from seeing stale 'true' values
    if (step === 3) {
      // Going back to fix Parakeet
      setParakeetDownloaded(false);
    } else if (step === 4) {
      // Going back to fix Summary
      setSummaryModelDownloaded(false);
    }
    goToStep(step);
  };

  const getStatusDisplay = (status: VerificationStatus) => {
    switch (status) {
      case 'checking':
        return { text: 'Checking...', color: 'text-gray-500' };
      case 'ready':
        return { text: 'Ready', color: 'text-green-600' };
      case 'missing':
        return { text: 'Missing', color: 'text-red-600' };
    }
  };

  const getStatusIcon = (status: VerificationStatus) => {
    switch (status) {
      case 'checking':
        return <Loader2 className="w-5 h-5 text-gray-400 animate-spin" />;
      case 'ready':
        return <CheckCircle2 className="w-5 h-5 text-green-600" />;
      case 'missing':
        return <XCircle className="w-5 h-5 text-red-600" />;
    }
  };

  const isVerifying = verification.parakeet === 'checking' || verification.summary === 'checking';
  const allReady = verification.parakeet === 'ready' && verification.summary === 'ready';

  const summaryItems = [
    {
      name: 'Transcription Model',
      status: getStatusDisplay(verification.parakeet),
      verificationStatus: verification.parakeet,
      fixStep: 3, // Go to Parakeet download step
    },
    {
      name: 'Summary Model',
      status: getStatusDisplay(verification.summary),
      verificationStatus: verification.summary,
      fixStep: 4, // Go to Summary download step
    },
  ];

  return (
    <OnboardingContainer
      title="All Set!"
      description="You're ready to start using Meetily"
      step={4}
      totalSteps={4}
      stepOffset={1}
    >
      <div className="flex flex-col items-center space-y-8">
        {/* Success Icon */}
        <div className="w-20 h-20 rounded-full bg-green-100 flex items-center justify-center">
          <CheckCircle2 className="w-12 h-12 text-green-600" />
        </div>

        {/* Configuration Summary */}
        <div className="w-full max-w-md bg-white rounded-2xl border border-neutral-200 p-6 space-y-4">
          <h3 className="font-semibold text-neutral-900 mb-4">Configuration Summary</h3>

          {summaryItems.map((item, index) => {
            return (
              <div key={index} className="flex items-center justify-between gap-3">
                <div className="flex items-center gap-3">
                  {getStatusIcon(item.verificationStatus)}
                  <div>
                    <p className="text-sm font-medium text-neutral-900">{item.name}</p>
                    <p className={`text-xs ${item.status.color}`}>{item.status.text}</p>
                  </div>
                </div>
                {item.verificationStatus === 'missing' && (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => handleGoBack(item.fixStep)}
                    className="text-xs"
                  >
                    Fix
                  </Button>
                )}
              </div>
            );
          })}
        </div>

        {/* Error Display */}
        {completionError && (
          <div className="w-full max-w-md bg-red-50 border border-red-200 rounded-lg p-4">
            <div className="flex items-start gap-3">
              <AlertCircle className="w-5 h-5 text-red-600 flex-shrink-0 mt-0.5" />
              <div className="flex-1">
                <h3 className="font-medium text-red-800 mb-1">Configuration Error</h3>
                <p className="text-sm text-red-700">{completionError}</p>
              </div>
            </div>
          </div>
        )}

        {/* Done Button */}
        <div className="w-full max-w-xs">
          <Button
            onClick={handleDone}
            disabled={isCompleting || isVerifying || !allReady}
            className="w-full h-12 text-base font-semibold bg-gray-900 hover:bg-gray-800 text-white disabled:opacity-50"
          >
            {isVerifying ? (
              <>
                <Loader2 className="w-4 h-4 mr-2 animate-spin" />
                Verifying...
              </>
            ) : isCompleting ? (
              <>
                <Loader2 className="w-4 h-4 mr-2 animate-spin" />
                Saving...
              </>
            ) : !allReady ? (
              'Models Missing'
            ) : (
              'Done!'
            )}
          </Button>
        </div>
      </div>
    </OnboardingContainer>
  );
}
