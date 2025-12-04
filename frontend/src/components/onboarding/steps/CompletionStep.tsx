import React, { useState } from 'react';
import { CheckCircle2, Mic, Sparkles, AlertCircle, Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { OnboardingContainer } from '../OnboardingContainer';
import { useOnboarding } from '@/contexts/OnboardingContext';

export function CompletionStep() {
  const { completeOnboarding, selectedSummaryModel } = useOnboarding();
  const [isCompleting, setIsCompleting] = useState(false);
  const [completionError, setCompletionError] = useState<string | null>(null);

  const handleDone = async () => {
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

  const installedModels = [
    {
      name: 'Transcription Model',
      status: 'Installed',
      icon: Mic,
    },
    {
      name: 'Summary Model',
      status: 'Installed',
      icon: Sparkles,
    },
  ];

  return (
    <OnboardingContainer
      title="All Set!"
      description="You're ready to start using Meetily"
      step={5}
    >
      <div className="flex flex-col items-center space-y-10">
        {/* Success Icon */}
        <div className="w-20 h-20 rounded-full bg-green-100 flex items-center justify-center">
          <CheckCircle2 className="w-12 h-12 text-green-600" />
        </div>

        {/* Installed Models Summary */}
        <div className="w-full max-w-md space-y-3">
          <h3 className="text-lg font-semibold text-gray-900 text-center mb-4">Setup Summary</h3>
          {installedModels.map((model, index) => {
            const Icon = model.icon;
            return (
              <div
                key={index}
                className="flex items-center justify-between bg-white rounded-lg border border-gray-200 p-4"
              >
                <div className="flex items-center gap-3">
                  <Icon className="w-6 h-6 text-gray-600" />
                  <span className="font-medium text-gray-900">{model.name}</span>
                </div>
                <div className="flex items-center gap-2">
                  <CheckCircle2 className="w-4 h-4 text-green-600" />
                  <span className="text-sm text-green-600 font-medium">{model.status}</span>
                </div>
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
            disabled={isCompleting}
            className="w-full h-11 bg-gray-900 hover:bg-gray-800 text-white disabled:opacity-50"
          >
            {isCompleting ? (
              <>
                <Loader2 className="w-4 h-4 mr-2 animate-spin" />
                Saving...
              </>
            ) : (
              'Done'
            )}
          </Button>
        </div>
      </div>
    </OnboardingContainer>
  );
}
