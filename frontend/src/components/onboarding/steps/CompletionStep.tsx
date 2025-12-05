import React, { useState } from 'react';
import { CheckCircle2, Mic, Sparkles, AlertCircle, Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { OnboardingContainer } from '../OnboardingContainer';
import { useOnboarding } from '@/contexts/OnboardingContext';

export function CompletionStep({ isMac }: { isMac: boolean }) {
  const { completeOnboarding, goNext } = useOnboarding();
  const [isCompleting, setIsCompleting] = useState(false);
  const [completionError, setCompletionError] = useState<string | null>(null);

  const handleDone = async () => {
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

  const summaryItems = [
    {
      name: 'Transcription Model',
      status: 'Ready',
      statusColor: 'text-green-600',
      icon: Mic,
      iconColor: 'text-green-600',
    },
    {
      name: 'Summary Model',
      status: 'Ready',
      statusColor: 'text-green-600',
      icon: Sparkles,
      iconColor: 'text-green-600',
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
            const Icon = item.icon;
            return (
              <div key={index} className="flex items-center gap-3">
                <CheckCircle2 className={`w-5 h-5 ${item.iconColor}`} />
                <div className="flex-1">
                  <p className="text-sm font-medium text-neutral-900">{item.name}</p>
                  <p className={`text-xs ${item.statusColor}`}>{item.status}</p>
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
            className="w-full h-12 text-base font-semibold bg-gray-900 hover:bg-gray-800 text-white disabled:opacity-50"
          >
            {isCompleting ? (
              <>
                <Loader2 className="w-4 h-4 mr-2 animate-spin" />
                Saving...
              </>
            ) : (
              'Done!'
            )}
          </Button>
        </div>
      </div>
    </OnboardingContainer>
  );
}
