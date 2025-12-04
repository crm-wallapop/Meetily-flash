'use client';

import React, { useEffect, useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Mic, Volume2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { PermissionRow } from './PermissionRow';
import { useOnboarding } from '@/contexts/OnboardingContext';

interface PermissionsStepProps {
  onComplete: () => void;
}

export function PermissionsStep({ onComplete }: PermissionsStepProps) {
  const { setPermissionStatus, permissions } = useOnboarding();
  const [isPending, setIsPending] = useState(false);

  // Check permissions - only logs current state, doesn't auto-authorize
  // Actual permission checks are done via explicit user actions (clicking Enable)
  const checkPermissions = useCallback(async () => {
    console.log('[PermissionsStep] Current permission states:');
    console.log(`  - Microphone: ${permissions.microphone}`);
    console.log(`  - System Audio: ${permissions.systemAudio}`);
  }, [permissions.microphone, permissions.systemAudio]);

  // Check permissions on mount
  useEffect(() => {
    checkPermissions();
  }, [checkPermissions]);

  // Request microphone permission
  const handleMicrophoneAction = async () => {
    if (permissions.microphone === 'denied') {
      // Try to open system settings
      try {
        await invoke('open_system_settings');
      } catch {
        alert('Please enable microphone access in System Preferences > Security & Privacy > Microphone');
      }
      return;
    }

    setIsPending(true);
    try {
      console.log('[PermissionsStep] Triggering microphone permission...');
      const granted = await invoke<boolean>('trigger_microphone_permission');
      console.log('[PermissionsStep] Microphone permission result:', granted);

      if (granted) {
        setPermissionStatus('microphone', 'authorized');
      } else {
        // Permission was denied or dialog was dismissed
        setPermissionStatus('microphone', 'denied');
      }
    } catch (err) {
      console.error('[PermissionsStep] Failed to request microphone permission:', err);
      setPermissionStatus('microphone', 'denied');
    } finally {
      setIsPending(false);
    }
  };

  // Request system audio permission
  const handleSystemAudioAction = async () => {
    if (permissions.systemAudio === 'denied') {
      try {
        await invoke('open_system_settings');
      } catch {
        alert('Please enable system audio access in System Preferences');
      }
      return;
    }

    setIsPending(true);
    try {
      console.log('[PermissionsStep] Triggering system audio permission...');
      // This creates a temporary Core Audio tap which triggers the permission dialog
      await invoke('trigger_system_audio_permission_command');

      // Wait for user to handle the dialog
      await new Promise(resolve => setTimeout(resolve, 2000));

      // If we reach here without error, assume permission was granted
      setPermissionStatus('systemAudio', 'authorized');
      console.log('[PermissionsStep] System audio permission granted');
    } catch (err) {
      console.error('[PermissionsStep] Failed to request system audio permission:', err);
      setPermissionStatus('systemAudio', 'denied');
    } finally {
      setIsPending(false);
    }
  };

  const allPermissionsGranted =
    permissions.microphone === 'authorized' &&
    permissions.systemAudio === 'authorized';

  const handleContinue = () => {
    if (allPermissionsGranted) {
      onComplete();
    }
  };

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="text-center mb-8">
        <h2 className="text-2xl font-semibold text-neutral-900 mb-2">Grant Permissions</h2>
        <p className="text-neutral-500">
          Meetily needs access to your microphone and system audio to record meetings
        </p>
      </div>

      {/* Permission Rows */}
      <div className="space-y-4 mb-8">
        {/* Microphone */}
        <PermissionRow
          icon={<Mic className="w-5 h-5" />}
          title="Microphone"
          description="Required to capture your voice during meetings"
          status={permissions.microphone}
          isPending={isPending}
          onAction={handleMicrophoneAction}
        />

        {/* System Audio */}
        <PermissionRow
          icon={<Volume2 className="w-5 h-5" />}
          title="System Audio"
          description="Click Enable to grant Audio Capture permission"
          status={permissions.systemAudio}
          isPending={isPending}
          onAction={handleSystemAudioAction}
        />
      </div>

      {/* Action Button */}
      <div className="mt-auto">
        <Button
          onClick={handleContinue}
          disabled={!allPermissionsGranted}
          className="w-full h-12 text-base"
        >
          Continue
        </Button>

        {!allPermissionsGranted && (
          <p className="text-xs text-center text-muted-foreground mt-4">
            Please grant both permissions to continue. Recording won't work without them.
          </p>
        )}
      </div>
    </div>
  );
}
