'use client';

import React from 'react';
import { PermissionsStep } from './PermissionsStep';
import { useOnboarding } from '@/contexts/OnboardingContext';

interface OnboardingFlowProps {
  onComplete: () => void;
}

export function OnboardingFlow({ onComplete }: OnboardingFlowProps) {
  const { completeOnboarding } = useOnboarding();

  const handleComplete = async () => {
    try {
      await completeOnboarding();
      onComplete();
    } catch (error) {
      console.error('[OnboardingFlow] Failed to complete onboarding:', error);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop with blur */}
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" />

      {/* Modal Dialog */}
      <div className="relative w-full max-w-lg mx-4 bg-white rounded-2xl shadow-2xl p-8 max-h-[90vh] overflow-y-auto">
        {/* App Logo/Title */}
        <div className="flex items-center justify-center gap-3 mb-6">
          <div className="w-10 h-10 bg-blue-600 rounded-xl flex items-center justify-center">
            <svg className="w-6 h-6 text-white" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 11a7 7 0 01-7 7m0 0a7 7 0 01-7-7m7 7v4m0 0H8m4 0h4m-4-8a3 3 0 01-3-3V5a3 3 0 116 0v6a3 3 0 01-3 3z" />
            </svg>
          </div>
          <span className="text-xl font-semibold text-neutral-900">Meetily</span>
        </div>

        {/* Permissions Step */}
        <PermissionsStep onComplete={handleComplete} />
      </div>
    </div>
  );
}
