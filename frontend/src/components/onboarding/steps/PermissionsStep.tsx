import React, { useEffect, useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { platform } from '@tauri-apps/plugin-os';
import { Mic, Volume2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { OnboardingContainer } from '../OnboardingContainer';
import { PermissionRow } from '../shared';
import { useOnboarding } from '@/contexts/OnboardingContext';

interface AudioDevice {
  name: string;
  device_type: 'Input' | 'Output';
}

export function PermissionsStep() {
  const { setPermissionStatus, setPermissionsSkipped, permissions, completeOnboarding } = useOnboarding();
  const [isPending, setIsPending] = useState(false);
  const [isMac, setIsMac] = useState(false);

  // Detect platform
  useEffect(() => {
    const checkPlatform = () => {
      try {
        const currentPlatform = platform();
        setIsMac(currentPlatform === 'macos');
      } catch (err) {
        console.error('Failed to detect platform:', err);
        // Fallback to user agent detection
        setIsMac(navigator.userAgent.includes('Mac'));
      }
    };
    checkPlatform();
  }, []);

  // Check permissions
  const checkPermissions = useCallback(async () => {
    console.log('[PermissionsStep] Checking permissions...');

    try {
      // Check microphone permission by trying to get input devices
      try {
        const devices = await invoke<AudioDevice[]>('get_audio_devices');
        const hasMicrophone = devices.some((d) => d.device_type === 'Input');
        const newMicStatus = hasMicrophone ? 'authorized' : 'not_determined';
        console.log(`[PermissionsStep] Microphone: ${newMicStatus} (found ${devices.filter(d => d.device_type === 'Input').length} input devices)`);
        setPermissionStatus('microphone', newMicStatus);
      } catch (err) {
        console.error('Failed to check microphone permission:', err);
        setPermissionStatus('microphone', 'not_determined');
      }

      // System Audio permission check
      // On macOS, we cannot check if Audio Capture permission is granted
      // until the user actually triggers a Core Audio tap
      const currentSystemAudio = permissions.systemAudio;
      const currentScreenRecording = permissions.screenRecording;

      if (!currentSystemAudio || currentSystemAudio === 'not_determined') {
        setPermissionStatus('systemAudio', 'not_determined');
      }
      console.log(`[PermissionsStep] System Audio: ${currentSystemAudio || 'not_determined'}`);

      // Screen Recording permission (macOS only)
      if (isMac && (!currentScreenRecording || currentScreenRecording === 'not_determined')) {
        setPermissionStatus('screenRecording', 'not_determined');
      }
      console.log(`[PermissionsStep] Screen Recording: ${currentScreenRecording || 'not_determined'}`);
    } catch (err) {
      console.error('Failed to check permissions:', err);
    }
  }, [isMac, setPermissionStatus, permissions.systemAudio, permissions.screenRecording]);

  // Check permissions on mount
  useEffect(() => {
    checkPermissions();
  }, [checkPermissions]);

  // Request microphone permission
  const handleMicrophoneAction = async () => {
    if (permissions.microphone === 'denied') {
      alert('Please enable microphone access in System Preferences > Security & Privacy > Microphone');
      return;
    }

    setIsPending(true);
    try {
      // Trigger the actual permission request dialog
      await invoke('trigger_microphone_permission');
      // Wait a bit for the permission dialog to be handled
      await new Promise(resolve => setTimeout(resolve, 1000));
      // Check permissions again
      await checkPermissions();
    } catch (err) {
      console.error('Failed to request microphone permission:', err);
    } finally {
      setIsPending(false);
    }
  };

  // Request system audio permission
  const handleSystemAudioAction = async () => {
    if (permissions.systemAudio === 'denied') {
      alert('Please enable system audio access in System Preferences');
      return;
    }

    setIsPending(true);
    try {
      console.log('[PermissionsStep] Triggering Audio Capture permission dialog...');
      // This creates a temporary Core Audio tap which triggers the permission dialog
      await invoke('trigger_system_audio_permission_command');

      // Wait for user to handle the dialog
      await new Promise(resolve => setTimeout(resolve, 2000));

      // If we reach here without error, assume permission was granted or already existed
      setPermissionStatus('systemAudio', 'authorized');
      setPermissionStatus('screenRecording', 'authorized'); // Core Audio implies this on macOS
      console.log('[PermissionsStep] Audio Capture permission triggered successfully');
    } catch (err) {
      console.error('[PermissionsStep] Failed to request system audio permission:', err);
      setPermissionStatus('systemAudio', 'denied');
    } finally {
      setIsPending(false);
    }
  };

  const handleFinish = async () => {
    try {
      await completeOnboarding();
      window.location.reload();
    } catch (error) {
      console.error('Failed to complete onboarding:', error);
    }
  };

  const handleSkip = async () => {
    setPermissionsSkipped(true);
    await handleFinish();
  };

  const allPermissionsGranted =
    permissions.microphone === 'authorized' &&
    permissions.systemAudio === 'authorized';

  return (
    <OnboardingContainer
      title="Grant Permissions"
      description="Meetily needs access to your microphone and system audio to record meetings"
      step={6}
      hideProgress={true}
      showNavigation={allPermissionsGranted}
      canGoNext={allPermissionsGranted}
    >
      <div className="max-w-lg mx-auto space-y-6">
        {/* Permission Rows */}
        <div className="space-y-4">
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

        {/* Action Buttons */}
        <div className="flex flex-col gap-3 pt-4">
          <Button onClick={handleFinish} disabled={!allPermissionsGranted} className="w-full h-11">
            Finish Setup
          </Button>

          <button
            onClick={handleSkip}
            className="text-sm text-neutral-500 hover:text-neutral-700 transition-colors"
          >
            I'll do this later
          </button>

          {!allPermissionsGranted && (
            <p className="text-xs text-center text-muted-foreground">
              Recording won't work without permissions. You can grant them later in settings.
            </p>
          )}
        </div>
      </div>
    </OnboardingContainer>
  );
}
