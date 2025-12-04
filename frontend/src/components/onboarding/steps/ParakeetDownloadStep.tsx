import React, { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Download, Loader2, CheckCircle2, AlertCircle } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Progress } from '@/components/ui/progress';
import { OnboardingContainer } from '../OnboardingContainer';
import { useOnboarding } from '@/contexts/OnboardingContext';
import { toast } from 'sonner';

const PARAKEET_MODEL = 'parakeet-tdt-0.6b-v3-int8';

export function ParakeetDownloadStep() {
  const {
    goNext,
    parakeetDownloaded,
    parakeetProgress,
    setParakeetDownloaded,
  } = useOnboarding();

  const [parakeetError, setParakeetError] = useState<string | null>(null);
  const [isChecking, setIsChecking] = useState(true);

  // Initialization effect
  useEffect(() => {
    initializeStep();
  }, []);

  // Auto-start download effect
  useEffect(() => {
    if (!isChecking && !parakeetDownloaded && !parakeetError) {
      downloadParakeet();
    }
  }, [isChecking, parakeetDownloaded, parakeetError]);


  const initializeStep = async () => {
    try {
      setIsChecking(true);
      console.log('[ParakeetDownloadStep] Initializing...');

      // Initialize Parakeet engine
      await invoke('parakeet_init');

      // Check if model already exists
      const exists = await invoke<boolean>('parakeet_has_available_models');
      console.log('[ParakeetDownloadStep] Model exists:', exists);

      if (exists) {
        setParakeetDownloaded(true);
        // Will auto-advance via useEffect
        return;
      }

      // Model not found, will trigger download via useEffect
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : 'Initialization failed';
      console.error('[ParakeetDownloadStep] Init error:', errorMsg);
      setParakeetError(errorMsg);
    } finally {
      setIsChecking(false);
    }
  };

  const downloadParakeet = async () => {
    try {
      setParakeetError(null);
      console.log('[ParakeetDownloadStep] Starting download...');

      await invoke('parakeet_download_model', {
        modelName: PARAKEET_MODEL,
      });

      // Download complete (context listener will set parakeetDownloaded)
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : 'Download failed';
      console.error('[ParakeetDownloadStep] Download error:', errorMsg);
      setParakeetError(errorMsg);
      toast.error('Failed to download Transcription model', {
        description: errorMsg,
      });
    }
  };

  return (
    <OnboardingContainer
      title="Step 1"
      description="Download transcription model (Parakeet v3 - open sourced by NVIDIA)"
      step={3}
    >
      <div className="flex flex-col items-center space-y-6">
        {/* Parakeet Download Card */}
        <div className="w-full max-w-md bg-white rounded-lg border border-gray-200 p-6 space-y-4">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              <Download className="w-5 h-5 text-gray-600" />
              <div>
                <h3 className="font-medium text-gray-900">Parakeet v3 </h3>
                <p className="text-sm text-gray-600">~670 MB</p>
              </div>
            </div>
            {isChecking ? (
              <Loader2 className="w-6 h-6 text-gray-400 animate-spin" />
            ) : parakeetDownloaded ? (
              <CheckCircle2 className="w-6 h-6 text-green-600" />
            ) : parakeetError ? (
              <AlertCircle className="w-6 h-6 text-red-600" />
            ) : (
              <Loader2 className="w-6 h-6 text-gray-600 animate-spin" />
            )}
          </div>

          {!parakeetDownloaded && parakeetProgress > 0 && (
            <div className="space-y-2">
              <Progress value={parakeetProgress} className="h-2" />
              <p className="text-xs text-center text-gray-500">
                {Math.round(parakeetProgress)}%
              </p>
            </div>
          )}

          {parakeetError && (
            <div className="bg-red-50 border border-red-200 rounded-lg p-3">
              <p className="text-sm text-red-800">{parakeetError}</p>
              <Button
                onClick={downloadParakeet}
                variant="outline"
                size="sm"
                className="mt-2 w-full border-red-300 text-red-700 hover:bg-red-100"
              >
                Retry Download
              </Button>
            </div>
          )}
        </div>

        {/* Continue Button */}
        <div className="w-full max-w-xs">
          <Button
            onClick={goNext}
            disabled={!parakeetDownloaded}
            className="w-full h-11 bg-gray-900 hover:bg-gray-800 text-white disabled:opacity-50 disabled:cursor-not-allowed"
          >
            Proceed to Step 2
          </Button>
        </div>
      </div>
    </OnboardingContainer>
  );
}
