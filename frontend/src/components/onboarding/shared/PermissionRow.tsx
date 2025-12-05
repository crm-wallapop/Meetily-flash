import React from 'react';
import { CheckCircle2, Loader2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import type { PermissionRowProps } from '@/types/onboarding';

export function PermissionRow({ icon, title, description, status, isPending = false, onAction }: PermissionRowProps) {
  const isAuthorized = status === 'authorized';
  const isDenied = status === 'denied';
  const isChecking = status === 'checking' || isPending;

  const getButtonText = () => {
    if (isChecking) return 'Checking...';
    if (isDenied) return 'Open Settings';
    return 'Enable';
  };

  return (
    <div
      className={cn(
        'flex items-center justify-between rounded-2xl border px-6 py-5',
        'transition-all duration-200',
        isAuthorized ? 'border-gray-900 bg-gray-50' : 'bg-white border-neutral-200'
      )}
    >
      {/* Left side: Icon + Info */}
      <div className="flex items-center gap-3 flex-1 min-w-0">
        {/* Icon */}
        <div
          className={cn(
            'flex size-10 items-center justify-center rounded-full flex-shrink-0',
            isAuthorized ? 'bg-gray-100' : 'bg-neutral-50'
          )}
        >
          <div className={cn(isAuthorized ? 'text-gray-900' : 'text-neutral-500')}> {icon}</div>
        </div>

        {/* Title + Description */}
        <div className="min-w-0 flex-1">
          <div className="font-medium truncate text-neutral-900">{title}</div>
          <div className="text-sm text-muted-foreground">
            {isAuthorized ? (
              <span className="text-gray-900 flex items-center gap-1">
                <CheckCircle2 className="w-3.5 h-3.5" />
                Access Granted
              </span>
            ) : (
              <span>{description}</span>
            )}
          </div>
        </div>
      </div>

      {/* Right side: Action button or checkmark */}
      <div className="flex items-center gap-2 flex-shrink-0 ml-3">
        {!isAuthorized && (
          <Button variant="outline" size="sm" onClick={onAction} disabled={isChecking} className="min-w-[100px]">
            {isChecking && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            {getButtonText()}
          </Button>
        )}
        {isAuthorized && (
          <div className="flex size-8 items-center justify-center rounded-full bg-green-100">
            <CheckCircle2 className="w-4 h-4 text-green-600" />
          </div>
        )}
      </div>
    </div>
  );
}
