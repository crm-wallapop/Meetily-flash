import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';

export interface PermissionStatus {
  hasMicrophone: boolean;
  hasSystemAudio: boolean;
  isChecking: boolean;
  error: string | null;
}

export function usePermissionCheck() {
  const [status, setStatus] = useState<PermissionStatus>({
    hasMicrophone: false,
    hasSystemAudio: false,
    isChecking: true,
    error: null,
  });

  const checkPermissions = async () => {
    setStatus(prev => ({ ...prev, isChecking: true, error: null }));

    try {
      // Get audio devices to check for microphone and system audio availability
      const devices = await invoke<Array<{ name: string; device_type: 'Input' | 'Output' }>>('get_audio_devices');

      // Check for microphone devices (Input)
      const inputDevices = devices.filter(d => d.device_type === 'Input');
      const hasMicrophone = inputDevices.length > 0;

      // Check for system audio devices (Output)
      // On macOS, we need ScreenCaptureKit devices for system audio
      const outputDevices = devices.filter(d => d.device_type === 'Output');
      const hasSystemAudio = outputDevices.length > 0;

      console.log('Permission check:', {
        hasMicrophone,
        hasSystemAudio,
        inputDevices: inputDevices.length,
        outputDevices: outputDevices.length
      });

      setStatus({
        hasMicrophone,
        hasSystemAudio,
        isChecking: false,
        error: null,
      });

      return { hasMicrophone, hasSystemAudio };
    } catch (error) {
      console.error('Failed to check audio permissions:', error);
      setStatus({
        hasMicrophone: false,
        hasSystemAudio: false,
        isChecking: false,
        error: error instanceof Error ? error.message : 'Failed to check permissions',
      });
      return { hasMicrophone: false, hasSystemAudio: false };
    }
  };

  const requestPermissions = async () => {
    try {
      // Trigger microphone permission dialog
      await invoke('trigger_microphone_permission');

      // Recheck after triggering
      setTimeout(() => {
        checkPermissions();
      }, 1000);
    } catch (error) {
      console.error('Failed to request permissions:', error);
    }
  };

  // Trigger permission dialog on mount to ensure we have microphone access
  useEffect(() => {
    const initPermissions = async () => {
      try {
        // First trigger the microphone permission dialog
        console.log('🎤 [usePermissionCheck] Triggering microphone permission on mount...');
        const result = await invoke('trigger_microphone_permission');
        console.log('🎤 [usePermissionCheck] trigger_microphone_permission result:', result);

        // Wait a moment for permission to be processed
        await new Promise(resolve => setTimeout(resolve, 500));

        // Then check permissions
        console.log('🎤 [usePermissionCheck] Checking permissions after trigger...');
        await checkPermissions();
      } catch (error) {
        console.error('🎤 [usePermissionCheck] Failed to trigger microphone permission:', error);
        // Still try to check permissions even if trigger fails
        await checkPermissions();
      }
    };

    initPermissions();
  }, []);

  return {
    ...status,
    checkPermissions,
    requestPermissions,
  };
}
