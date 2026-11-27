import React, { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Mic, Volume2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { OnboardingContainer } from '../OnboardingContainer';
import { PermissionRow } from '../shared';
import { useOnboarding } from '@/contexts/OnboardingContext';

const permissionChecks = {
  microphone: {
    command: 'trigger_microphone_permission',
    pane: 'Privacy_Microphone',
    settingsLabel: 'Microphone',
  },
  systemAudio: {
    command: 'trigger_system_audio_permission_command',
    pane: 'Privacy_ScreenCapture',
    settingsLabel: 'Screen & System Audio Recording',
  },
} as const;

type AudioPermission = keyof typeof permissionChecks;

export function PermissionsStep() {
  const { setPermissionStatus, setPermissionsSkipped, permissions, completeOnboarding } = useOnboarding();
  const [isPending, setIsPending] = useState(false);

  const checkPermission = async (permission: AudioPermission) => {
    setIsPending(true);
    try {
      const granted = await invoke<boolean>(permissionChecks[permission].command);
      const ungranted = permission === 'microphone' ? 'denied' : 'unverified';
      setPermissionStatus(permission, granted ? 'authorized' : ungranted);
    } catch (error) {
      console.error(`[PermissionsStep] Could not check ${permission} access:`, error);
      setPermissionStatus(permission, 'unverified');
    } finally {
      setIsPending(false);
    }
  };

  const handlePermissionAction = async (permission: AudioPermission) => {
    const status = permissions[permission];
    if (status !== 'denied' && status !== 'unverified') {
      await checkPermission(permission);
      return;
    }

    const { pane, settingsLabel } = permissionChecks[permission];
    try {
      await invoke('open_system_settings', { preferencePane: pane });
    } catch {
      alert(`Open System Settings > Privacy & Security > ${settingsLabel} to allow access.`);
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
      step={4}
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
            description={permissions.microphone === 'unverified'
              ? 'Meetily could not verify microphone access.'
              : 'Required to capture your voice during meetings'}
            status={permissions.microphone}
            isPending={isPending}
            onAction={() => handlePermissionAction('microphone')}
            onRetry={() => checkPermission('microphone')}
          />

          {/* System Audio */}
          <PermissionRow
            icon={<Volume2 className="w-5 h-5" />}
            title="System Audio"
            description={permissions.systemAudio === 'unverified'
              ? 'Meetily did not verify system audio access. Check access in System Settings or play audio and check again.'
              : 'Play audio on this Mac while Meetily checks access.'}
            status={permissions.systemAudio}
            isPending={isPending}
            onAction={() => handlePermissionAction('systemAudio')}
            onRetry={() => checkPermission('systemAudio')}
          />
        </div>

        {/* Action Buttons */}
        <div className="flex flex-col gap-3 pt-4">
          <Button onClick={handleFinish} disabled={!allPermissionsGranted} className="w-full h-11">
            Finish Setup
          </Button>

          <button
            onClick={handleSkip}
            className="text-sm text-neutral-500 dark:text-ink-400 hover:text-neutral-700 dark:hover:text-ink-200 transition-colors"
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
