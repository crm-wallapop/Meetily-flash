import React, { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Download, Loader2, CheckCircle2, AlertCircle } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Progress } from '@/components/ui/progress';
import { OnboardingContainer } from '../OnboardingContainer';
import { useOnboarding } from '@/contexts/OnboardingContext';
import { toast } from 'sonner';

const PARAKEET_MODEL = 'parakeet-tdt-0.6b-v3-int8';

export function ModelDownloadStep() {
  const {
    goNext,
    parakeetDownloaded,
    parakeetProgress,
    summaryModelDownloaded,
    summaryModelProgress,
    selectedSummaryModel,
    setParakeetDownloaded,
    setSummaryModelDownloaded,
    setSelectedSummaryModel,
  } = useOnboarding();

  const [parakeetError, setParakeetError] = useState<string | null>(null);
  const [summaryModelError, setSummaryModelError] = useState<string | null>(null);
  const [isChecking, setIsChecking] = useState(true);
  // Initialize as null to prevent premature downloads
  const [recommendedModel, setRecommendedModel] = useState<string | null>(null);
  const [modelDisplayName, setModelDisplayName] = useState<string>('Gemma 3 1B');
  const [modelSize, setModelSize] = useState<string>('~806 MB');

  // Combined initialization effect
  useEffect(() => {
    initializeStep();
  }, []);

  const initializeStep = async () => {
    try {
      setIsChecking(true);
      
      // 1. Detect recommended model
      let modelToUse = 'gemma3:1b'; // Default fallback
      try {
        const recommended = await invoke<string>('builtin_ai_get_recommended_model');
        console.log('[ModelDownloadStep] Recommended summary model:', recommended);
        modelToUse = recommended;
      } catch (error) {
        console.error('[ModelDownloadStep] Failed to detect recommended model:', error);
      }
      
      setRecommendedModel(modelToUse);
      
      // Set display info based on available Gemma models
      if (modelToUse === 'gemma3:4b') {
        setModelDisplayName('Gemma 3 4B (Balanced)');
        setModelSize('~2.4 GB');
      } else {
        setModelDisplayName('Gemma 3 1B (Fast)');
        setModelSize('~1 GB');
      }

      // 2. Check existing models
      console.log('[ModelDownloadStep] Checking for existing models...');
      
      // Initialize Parakeet engine
      await invoke('parakeet_init');
      
      // Check Parakeet
      const parakeetExists = await invoke<boolean>('parakeet_has_available_models');
      console.log('[ModelDownloadStep] Parakeet available:', parakeetExists);
      if (parakeetExists) {
        setParakeetDownloaded(true);
      }

      // Check Summary Model
      // First check if ANY summary model exists
      const existingModel = await invoke<string | null>('builtin_ai_get_available_summary_model');
      
      let summaryReady = false;
      if (existingModel) {
        console.log(`[ModelDownloadStep] Found existing model: ${existingModel}, using it instead of recommended ${modelToUse}`);
        setSelectedSummaryModel(existingModel);
        setSummaryModelDownloaded(true);
        summaryReady = true;
        
        // Update display if we found a different model than recommended
        if (existingModel !== modelToUse) {
          if (existingModel === 'gemma3:4b') {
            setModelDisplayName('Gemma 3 4B (Balanced)');
            setModelSize('~2.4 GB');
          } else {
            setModelDisplayName('Gemma 3 1B (Fast)');
            setModelSize('~1 GB');
          }
        }
      } else {
        // Check if the recommended model is ready (should be false if we just checked all, but good for safety)
        summaryReady = await invoke<boolean>('builtin_ai_is_model_ready', {
          modelName: modelToUse,
          refresh: true,
        });
        console.log(`[ModelDownloadStep] ${modelToUse} ready:`, summaryReady);
        
        if (summaryReady) {
          setSummaryModelDownloaded(true);
          setSelectedSummaryModel(modelToUse);
        } else {
          // If not ready, ensure we set the selected model to the recommended one for download
          setSelectedSummaryModel(modelToUse);
        }
      }

      // Auto-advance if everything is ready
      if (parakeetExists && summaryReady) {
        console.log('[ModelDownloadStep] All models available, advancing...');
        setTimeout(() => goNext(), 1000);
      }

    } catch (err) {
      console.error('[ModelDownloadStep] Initialization error:', err);
    } finally {
      setIsChecking(false);
    }
  };

  // Auto-start downloads when ready
  useEffect(() => {
    if (!isChecking && !parakeetDownloaded && !parakeetError) {
      downloadParakeet();
    }
  }, [isChecking, parakeetDownloaded, parakeetError]);

  useEffect(() => {
    // Only download if:
    // 1. Not currently checking
    // 2. Parakeet is done (sequential download)
    // 3. Summary model not yet downloaded
    // 4. No error
    // 5. We have a recommended model identified
    if (!isChecking && parakeetDownloaded && !summaryModelDownloaded && !summaryModelError && recommendedModel) {
      downloadSummaryModel();
    }
  }, [isChecking, parakeetDownloaded, summaryModelDownloaded, summaryModelError, recommendedModel]);

  const downloadParakeet = async () => {
    try {
      setParakeetError(null);
      console.log('[ModelDownloadStep] Starting Parakeet download');

      await invoke('parakeet_download_model', {
        modelName: PARAKEET_MODEL,
      });
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : 'Download failed';
      console.error('[ModelDownloadStep] Parakeet download error:', errorMsg);
      setParakeetError(errorMsg);
      toast.error('Failed to download Transcription model', {
        description: errorMsg,
      });
    }
  };

  const downloadSummaryModel = async () => {
    if (!recommendedModel) return;

    try {
      setSummaryModelError(null);
      // Use selectedSummaryModel if set (e.g. from existing check), otherwise recommended
      const modelToDownload = selectedSummaryModel || recommendedModel;
      console.log(`[ModelDownloadStep] Starting ${modelToDownload} download`);

      await invoke('builtin_ai_download_model', {
        modelName: modelToDownload,
      });
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : 'Download failed';
      console.error(`[ModelDownloadStep] Summary model download error:`, errorMsg);
      setSummaryModelError(errorMsg);
      toast.error('Failed to download Summary model', {
        description: errorMsg,
      });
    }
  };

  const bothDownloaded = parakeetDownloaded && summaryModelDownloaded;

  return (
    <OnboardingContainer
      title="Downloading Models"
      description="Please wait while we download the required AI models"
      step={4}
    >
      <div className="flex flex-col items-center space-y-6">
        {/* Parakeet Download Card */}
        <div className="w-full max-w-md bg-white rounded-lg border border-gray-200 p-6 space-y-4">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              <Download className="w-5 h-5 text-gray-600" />
              <div>
                <h3 className="font-medium text-gray-900">Transcription Model</h3>
                <p className="text-sm text-gray-600">Parakeet v3 (~670 MB)</p>
              </div>
            </div>
            {isChecking ? (
              <Loader2 className="w-6 h-6 text-gray-400 animate-spin" />
            ) : parakeetDownloaded ? (
              <CheckCircle2 className="w-6 h-6 text-green-600" />
            ) : parakeetProgress > 0 ? (
              <Loader2 className="w-6 h-6 text-gray-600 animate-spin" />
            ) : parakeetError ? (
              <AlertCircle className="w-6 h-6 text-red-600" />
            ) : null}
          </div>

          {!parakeetDownloaded && parakeetProgress > 0 && (
            <div className="space-y-2">
              <Progress value={parakeetProgress} className="h-2" />
              <p className="text-xs text-center text-gray-500">{Math.round(parakeetProgress)}%</p>
            </div>
          )}

          {parakeetError && (
            <div className="bg-red-50 border border-red-200 rounded-lg p-3">
              <p className="text-sm text-red-800">{parakeetError}</p>
              <Button
                onClick={downloadParakeet}
                variant="outline"
                size="sm"
                className="mt-2 w-full"
              >
                Retry
              </Button>
            </div>
          )}
        </div>

        {/* Summary Model Download Card */}
        <div className="w-full max-w-md bg-white rounded-lg border border-gray-200 p-6 space-y-4">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              <Download className="w-5 h-5 text-gray-600" />
              <div>
                <h3 className="font-medium text-gray-900">Summary Model</h3>
                <p className="text-sm text-gray-600">
                  {modelDisplayName} ({modelSize})
                </p>
              </div>
            </div>
            {isChecking ? (
              <Loader2 className="w-6 h-6 text-gray-400 animate-spin" />
            ) : summaryModelDownloaded ? (
              <CheckCircle2 className="w-6 h-6 text-green-600" />
            ) : summaryModelProgress > 0 ? (
              <Loader2 className="w-6 h-6 text-gray-600 animate-spin" />
            ) : summaryModelError ? (
              <AlertCircle className="w-6 h-6 text-red-600" />
            ) : !parakeetDownloaded ? (
              <p className="text-sm text-gray-500">Waiting...</p>
            ) : null}
          </div>

          {!summaryModelDownloaded && summaryModelProgress > 0 && (
            <div className="space-y-2">
              <Progress value={summaryModelProgress} className="h-2" />
              <p className="text-xs text-center text-gray-500">{Math.round(summaryModelProgress)}%</p>
            </div>
          )}

          {summaryModelError && (
            <div className="bg-red-50 border border-red-200 rounded-lg p-3">
              <p className="text-sm text-red-800">{summaryModelError}</p>
              <Button
                onClick={downloadSummaryModel}
                variant="outline"
                size="sm"
                className="mt-2 w-full"
              >
                Retry
              </Button>
            </div>
          )}

          {!parakeetDownloaded && !parakeetError && !isChecking && (
            <p className="text-sm text-gray-500">Waiting for Transcription model...</p>
          )}
        </div>

        {/* Next Button */}
        <div className="w-full max-w-xs">
          <Button
            onClick={goNext}
            disabled={!bothDownloaded}
            className="w-full h-11 bg-gray-900 hover:bg-gray-800 text-white disabled:opacity-50 disabled:cursor-not-allowed"
          >
            Almost There
          </Button>
        </div>
      </div>
    </OnboardingContainer>
  );
}
