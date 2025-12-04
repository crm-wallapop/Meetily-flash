import React from 'react';
import { cn } from '@/lib/utils';

interface OnboardingContainerProps {
  title: string;
  description?: string;
  children: React.ReactNode;
  step: number;
  totalSteps?: number;
  className?: string;
}

export function OnboardingContainer({
  title,
  description,
  children,
  step,
  totalSteps = 5,
  className,
}: OnboardingContainerProps) {
  return (
    <div className="fixed inset-0 bg-gray-50 flex items-center justify-center z-50 overflow-hidden">
      <div className={cn('w-full max-w-2xl h-full max-h-screen flex flex-col px-6 py-8', className)}>
        {/* Progress Indicator */}
        <div className="mb-8 flex-shrink-0">
          <div className="flex justify-center gap-2">
            {Array.from({ length: totalSteps }, (_, i) => (
              <div
                key={i}
                className={cn(
                  'h-1.5 flex-1 rounded-full transition-all duration-300',
                  i < step ? 'bg-gray-700' : 'bg-gray-200'
                )}
              />
            ))}
          </div>
        </div>

        {/* Header */}
        <div className="mb-8 text-center space-y-3 flex-shrink-0">
          <h1 className="text-4xl font-semibold text-gray-900 animate-fade-in-up">{title}</h1>
          {description && (
            <p className="text-base text-gray-600 max-w-md mx-auto animate-fade-in-up delay-75">
              {description}
            </p>
          )}
        </div>

        {/* Content - Scrollable */}
        <div className="flex-1 overflow-y-auto pr-2">
          <div className="space-y-6">{children}</div>
        </div>
      </div>
    </div>
  );
}
