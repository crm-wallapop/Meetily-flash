import React, { useEffect } from 'react';
import { useOnboarding } from '@/contexts/OnboardingContext';
import {
  WelcomeStep,
  SetupOverviewStep,
  ParakeetDownloadStep,
  SummaryModelDownloadStep,
  CompletionStep,
} from './steps';

interface OnboardingFlowProps {
  onComplete: () => void;
}

export function OnboardingFlow({ onComplete }: OnboardingFlowProps) {
  const { currentStep } = useOnboarding();

  // When Done is clicked on completion step, call parent callback
  useEffect(() => {
    // Listen for completion - we'll trigger this from CompletionStep
    // The actual completion is handled by completeOnboarding() in the context
    // This effect is just for cleanup/notification to parent
  }, [currentStep, onComplete]);

  return (
    <div className="onboarding-flow">
      {currentStep === 1 && <WelcomeStep />}
      {currentStep === 2 && <SetupOverviewStep />}
      {currentStep === 3 && <ParakeetDownloadStep />}
      {currentStep === 4 && <SummaryModelDownloadStep />}
      {currentStep === 5 && <CompletionStep />}
    </div>
  );
}
