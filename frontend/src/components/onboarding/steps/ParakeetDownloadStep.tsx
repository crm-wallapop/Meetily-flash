import React, { useEffect, useState, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Loader2, CheckCircle2, AlertCircle, Zap, RefreshCw } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Progress } from '@/components/ui/progress';
import { OnboardingContainer } from '../OnboardingContainer';
import { useOnboarding } from '@/contexts/OnboardingContext';
import { toast } from 'sonner';

const PARAKEET_MODEL = 'parakeet-tdt-0.6b-v3-int8';

type ModelStatus = 'checking' | 'ready' | 'downloading' | 'downloaded' | 'error';

export function ParakeetDownloadStep() {
  const {
    goNext,
    parakeetDownloaded,
    parakeetProgress,
    parakeetProgressInfo,
    setParakeetDownloaded,
  } = useOnboarding();

  const [status, setStatus] = useState<ModelStatus>('checking');
  const [parakeetError, setParakeetError] = useState<string | null>(null);
  const [retryCount, setRetryCount] = useState(0);
  // Track if we've verified model is NOT ready (to prevent context race condition)
  const [verifiedNotReady, setVerifiedNotReady] = useState(false);
  // Ref to track latest progress (avoids stale closure issues)
  const progressRef = useRef(parakeetProgress);

  // Keep progress ref updated
  useEffect(() => {
    progressRef.current = parakeetProgress;
  }, [parakeetProgress]);

  // Initialization effect
  useEffect(() => {
    initializeStep();
  }, []);

  // Sync status with context (but respect verification result)
  useEffect(() => {
    // Don't sync while checking
    if (status === 'checking') {
      return;
    }
    // Don't sync if we just verified the model is NOT ready
    // (context update is async and might still have stale true value)
    if (verifiedNotReady) {
      return;
    }
    // Only sync downloaded state from context if we didn't just verify it's missing
    if (parakeetDownloaded) {
      setStatus('downloaded');
    } else if (parakeetProgress > 0 && status !== 'error') {
      setStatus('downloading');
    }
  }, [parakeetDownloaded, parakeetProgress, status, verifiedNotReady]);

  // Auto-start download effect
  useEffect(() => {
    if (status === 'ready' && !parakeetError) {
      downloadParakeet();
    }
  }, [status, parakeetError]);

  // Reset verifiedNotReady flag when download actually completes
  useEffect(() => {
    if (parakeetProgress >= 100 && verifiedNotReady) {
      console.log('[ParakeetDownloadStep] Download complete, resetting verifiedNotReady flag');
      setVerifiedNotReady(false);
      setStatus('downloaded');
      setParakeetDownloaded(true);
    }
  }, [parakeetProgress, verifiedNotReady]);

  const initializeStep = async () => {
    try {
      setStatus('checking');
      setVerifiedNotReady(false); // Reset flag at start of verification
      console.log('[ParakeetDownloadStep] Initializing...');

      // Check if a download is already in progress (e.g., user clicked "Fix" to come back)
      // Use ref to get latest value (avoids stale closure)
      const currentProgress = progressRef.current;
      if (currentProgress > 0 && currentProgress < 100) {
        console.log('[ParakeetDownloadStep] Download already in progress at', currentProgress, '%, continuing...');
        setStatus('downloading');
        return;
      }

      // Initialize Parakeet engine
      await invoke('parakeet_init');

      // Check if model already exists
      const exists = await invoke<boolean>('parakeet_has_available_models');
      console.log('[ParakeetDownloadStep] Model exists:', exists);

      if (exists) {
        setParakeetDownloaded(true);
        setStatus('downloaded');
        return;
      }

      // Model NOT found, set to ready (will trigger download)
      // IMPORTANT: Set flag BEFORE updating context to prevent race condition
      // The sync effect will ignore stale context values while this flag is true
      setVerifiedNotReady(true);
      setParakeetDownloaded(false);
      setStatus('ready');
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : 'Initialization failed';
      console.error('[ParakeetDownloadStep] Init error:', errorMsg);
      setParakeetError(errorMsg);
      setStatus('error');
    }
  };

  const downloadParakeet = async () => {
    try {
      setParakeetError(null);
      setStatus('downloading');
      console.log('[ParakeetDownloadStep] Starting download...');

      await invoke('parakeet_download_model', {
        modelName: PARAKEET_MODEL,
      });

      // Download complete (context listener will set parakeetDownloaded)
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : 'Download failed';
      console.error('[ParakeetDownloadStep] Download error:', errorMsg);
      setParakeetError(errorMsg);
      setStatus('error');
      toast.error('Failed to download Transcription model', {
        description: errorMsg,
      });
    }
  };

  const retryDownload = async () => {
    setRetryCount((prev: number) => prev + 1);
    setParakeetError(null);
    setStatus('ready');
  };

  const isDownloading = status === 'downloading';
  const isDownloaded = status === 'downloaded';
  const isError = status === 'error';
  const isChecking = status === 'checking';

  return (
    <OnboardingContainer
      title="Step 1"
      description="Download Transcription Model (Parakeet V3 - open source model from NVIDIA)"
      step={2}
      totalSteps={4}
      stepOffset={1}
    >
      <div className="flex flex-col items-center space-y-6">
        {/* Success State */}
        {isDownloaded && (
          <div className="w-full max-w-md bg-green-50 rounded-lg border border-green-200 p-6 text-center space-y-2">
            <CheckCircle2 className="w-12 h-12 text-green-600 mx-auto" />
            <h3 className="font-semibold text-gray-900">Model Ready!</h3>
            <p className="text-sm text-gray-600">Transcription model is ready to use</p>
          </div>
        )}

        {/* Checking State */}
        {isChecking && (
          <div className="w-full max-w-md bg-white rounded-lg border border-gray-200 p-6 space-y-4">
            <div className="flex items-center gap-3">
              <div className="flex size-12 items-center justify-center rounded-lg bg-gray-100">
                <Zap className="w-6 h-6 text-gray-700" />
              </div>
              <div className="flex-1">
                <h3 className="font-semibold text-gray-900">Parakeet v3</h3>
                <p className="text-sm text-gray-600">Checking model status...</p>
              </div>
              <Loader2 className="w-5 h-5 animate-spin text-gray-400" />
            </div>
          </div>
        )}

        {/* Downloading State */}
        {isDownloading && (
          <div className="w-full max-w-md bg-gray-50 rounded-lg border border-gray-200 p-6 space-y-3">
            <div className="flex items-center gap-3">
              <Loader2 className="w-5 h-5 animate-spin text-gray-900" />
              <div className="flex-1">
                <h3 className="font-semibold text-gray-900">Downloading Transcription Model</h3>
                <p className="text-sm text-gray-600">
                  {parakeetProgressInfo.totalMb > 0 ? (
                    <>
                      {parakeetProgressInfo.downloadedMb.toFixed(1)} MB / {parakeetProgressInfo.totalMb.toFixed(1)} MB
                      {parakeetProgressInfo.speedMbps > 0 && (
                        <span className="ml-2 text-gray-500">
                          ({parakeetProgressInfo.speedMbps.toFixed(1)} MB/s)
                        </span>
                      )}
                    </>
                  ) : (
                    '~670 MB'
                  )}
                </p>
              </div>
              <span className="text-sm font-medium text-gray-900">{Math.round(parakeetProgress)}%</span>
            </div>
            <Progress value={parakeetProgress} className="h-2" />
          </div>
        )}

        {/* Error State */}
        {isError && (
          <div className="w-full max-w-md bg-white rounded-lg border border-gray-200 p-6 space-y-4">
            <div className="flex items-center gap-3">
              <div className="flex size-12 items-center justify-center rounded-lg bg-red-50">
                <AlertCircle className="w-6 h-6 text-red-600" />
              </div>
              <div className="flex-1">
                <h3 className="font-semibold text-gray-900">Parakeet v3</h3>
                <p className="text-sm text-gray-600">~670 MB</p>
              </div>
            </div>

            <div className="flex items-start gap-2 text-sm text-red-600 bg-red-50 p-3 rounded-lg">
              <AlertCircle className="w-4 h-4 mt-0.5 flex-shrink-0" />
              <div className="flex-1">
                <p>{parakeetError}</p>
                {retryCount > 0 && (
                  <p className="mt-1 text-xs">Retry attempt: {retryCount}</p>
                )}
              </div>
              <button
                onClick={retryDownload}
                className="p-1.5 hover:bg-red-100 rounded transition-colors"
                title="Retry download"
              >
                <RefreshCw className="w-4 h-4" />
              </button>
            </div>
          </div>
        )}

        {/* Continue Button */}
        <div className="w-full max-w-xs">
          <Button
            onClick={goNext}
            disabled={!isDownloaded}
            className="w-full h-11 bg-gray-900 hover:bg-gray-800 text-white disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isDownloaded ? (
              <>
                <CheckCircle2 className="w-4 h-4 mr-2" />
                Proceed to Step 2
              </>
            ) : isDownloading ? (
              <>
                <Loader2 className="w-4 h-4 mr-2 animate-spin" />
                Downloading...
              </>
            ) : (
              'Waiting for Download'
            )}
          </Button>
        </div>

      </div>
    </OnboardingContainer>
  );
}
