/**
 * Permission status for individual permissions
 */
export type PermissionStatus = 'not_determined' | 'authorized' | 'denied';

/**
 * Permissions tracking for onboarding
 */
export interface Permissions {
  microphone: PermissionStatus;
  systemAudio: PermissionStatus;
}

/**
 * Onboarding status stored in JSON
 */
export interface OnboardingStatus {
  version: string;
  completed: boolean;
  permissions: Permissions;
  lastUpdated: string;
}

/**
 * Default onboarding status for new users
 */
export const DEFAULT_ONBOARDING_STATUS: OnboardingStatus = {
  version: '1.0',
  completed: false,
  permissions: {
    microphone: 'not_determined',
    systemAudio: 'not_determined',
  },
  lastUpdated: new Date().toISOString(),
};
