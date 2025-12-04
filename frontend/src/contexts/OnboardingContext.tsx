'use client';

import React, { createContext, useContext, useState, useCallback, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { OnboardingStatus, PermissionStatus, Permissions } from '@/types/onboarding';
import { DEFAULT_ONBOARDING_STATUS } from '@/types/onboarding';

interface OnboardingContextType {
  // Status
  isLoading: boolean;
  isOnboardingComplete: boolean;
  permissions: Permissions;

  // Actions
  setPermissionStatus: (permission: keyof Permissions, status: PermissionStatus) => void;
  completeOnboarding: () => Promise<void>;
  resetOnboarding: () => Promise<void>;
}

const OnboardingContext = createContext<OnboardingContextType | undefined>(undefined);

export function OnboardingProvider({ children }: { children: React.ReactNode }) {
  const [isLoading, setIsLoading] = useState(true);
  const [isOnboardingComplete, setIsOnboardingComplete] = useState(false);
  const [permissions, setPermissions] = useState<Permissions>(DEFAULT_ONBOARDING_STATUS.permissions);
  const saveTimeoutRef = useRef<NodeJS.Timeout>();
  const statusLoadedRef = useRef(false);

  // Load status from Rust on mount
  useEffect(() => {
    async function loadStatus() {
      try {
        console.log('[Onboarding] Loading status from Rust...');
        const status = await invoke<OnboardingStatus | null>('get_onboarding_status');

        if (status) {
          console.log('[Onboarding] Loaded status:', status);
          setIsOnboardingComplete(status.completed);
          setPermissions(status.permissions);
        } else {
          console.log('[Onboarding] No saved status, using defaults');
          setIsOnboardingComplete(false);
          setPermissions(DEFAULT_ONBOARDING_STATUS.permissions);
        }
      } catch (error) {
        console.error('[Onboarding] Failed to load status:', error);
        // Continue with defaults
        setIsOnboardingComplete(false);
        setPermissions(DEFAULT_ONBOARDING_STATUS.permissions);
      } finally {
        statusLoadedRef.current = true;
        setIsLoading(false);
      }
    }

    loadStatus();
  }, []);

  // Auto-save to file when permissions change (debounced)
  useEffect(() => {
    if (!statusLoadedRef.current) return; // Don't save during initial load

    // Clear existing timeout
    if (saveTimeoutRef.current) {
      clearTimeout(saveTimeoutRef.current);
    }

    // Debounce save by 500ms
    saveTimeoutRef.current = setTimeout(async () => {
      const status: OnboardingStatus = {
        version: '1.0',
        completed: isOnboardingComplete,
        permissions,
        lastUpdated: new Date().toISOString(),
      };

      try {
        console.log('[Onboarding] Saving status:', status);
        await invoke('save_onboarding_status_cmd', { status });
      } catch (error) {
        console.error('[Onboarding] Failed to save status:', error);
      }
    }, 500);

    return () => {
      if (saveTimeoutRef.current) {
        clearTimeout(saveTimeoutRef.current);
      }
    };
  }, [permissions, isOnboardingComplete]);

  const setPermissionStatus = useCallback(
    (permission: keyof Permissions, status: PermissionStatus) => {
      console.log(`[Onboarding] Setting ${permission} to ${status}`);
      setPermissions((prev) => ({
        ...prev,
        [permission]: status,
      }));
    },
    []
  );

  const completeOnboarding = useCallback(async () => {
    try {
      console.log('[Onboarding] Completing onboarding...');
      await invoke('complete_onboarding');
      setIsOnboardingComplete(true);
      console.log('[Onboarding] Onboarding completed successfully');
    } catch (error) {
      console.error('[Onboarding] Failed to complete onboarding:', error);
      throw error;
    }
  }, []);

  const resetOnboarding = useCallback(async () => {
    try {
      console.log('[Onboarding] Resetting onboarding...');
      await invoke('reset_onboarding_status_cmd');
      setIsOnboardingComplete(false);
      setPermissions(DEFAULT_ONBOARDING_STATUS.permissions);
      console.log('[Onboarding] Onboarding reset successfully');
    } catch (error) {
      console.error('[Onboarding] Failed to reset onboarding:', error);
      throw error;
    }
  }, []);

  const value: OnboardingContextType = {
    isLoading,
    isOnboardingComplete,
    permissions,
    setPermissionStatus,
    completeOnboarding,
    resetOnboarding,
  };

  return <OnboardingContext.Provider value={value}>{children}</OnboardingContext.Provider>;
}

export function useOnboarding() {
  const context = useContext(OnboardingContext);
  if (!context) {
    throw new Error('useOnboarding must be used within OnboardingProvider');
  }
  return context;
}
