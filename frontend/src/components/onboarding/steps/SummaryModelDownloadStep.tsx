import React, { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Loader2, CheckCircle2, AlertCircle, Sparkles, RefreshCw } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Progress } from '@/components/ui/progress';
import { OnboardingContainer } from '../OnboardingContainer';
import { useOnboarding } from '@/contexts/OnboardingContext';
import { toast } from 'sonner';

const MODEL_DISPLAY_INFO: Record<string, { name: string; size: string }> = {
  'gemma3:1b': { name: 'Gemma 3 1B', size: '~806 MB' },
  'gemma3:4b': { name: 'Gemma 3 4B', size: '~2.5 GB' },
  'mistral:7b': { name: 'Mistral 7B', size: '~4.3 GB' },
};

type ModelStatus = 'checking' | 'ready' | 'downloading' | 'downloaded' | 'error';

export function SummaryModelDownloadStep() {
  const {
    goNext,
    summaryModelDownloaded,
    summaryModelProgress,
    selectedSummaryModel,
    setSummaryModelDownloaded,
    setSelectedSummaryModel,
  } = useOnboarding();

  const [status, setStatus] = useState<ModelStatus>('checking');
  const [summaryModelError, setSummaryModelError] = useState<string | null>(null);
  const [recommendedModel, setRecommendedModel] = useState<string | null>(null);
  const [modelDisplayName, setModelDisplayName] = useState<string>('');
  const [modelSize, setModelSize] = useState<string>('');
  const [retryCount, setRetryCount] = useState(0);

  // Initialization effect
  useEffect(() => {
    initializeStep();
  }, []);

  // Sync status with context
  useEffect(() => {
    if (summaryModelDownloaded) {
      setStatus('downloaded');
    } else if (summaryModelProgress > 0 && status !== 'error') {
      setStatus('downloading');
    }
  }, [summaryModelDownloaded, summaryModelProgress]);

  // Auto-start download effect
  useEffect(() => {
    if (status === 'ready' && !summaryModelError && recommendedModel) {
      downloadSummaryModel();
    }
  }, [status, summaryModelError, recommendedModel]);

  const updateDisplayInfo = (modelName: string) => {
    const info = MODEL_DISPLAY_INFO[modelName];
    if (info) {
      setModelDisplayName(info.name);
      setModelSize(info.size);
    } else {
      console.warn(`[SummaryModelDownloadStep] Unknown model: ${modelName}`);
      setModelDisplayName(modelName);
      setModelSize('Size unknown');
    }
  };

  const initializeStep = async () => {
    try {
      setStatus('checking');
      console.log('[SummaryModelDownloadStep] Initializing...');

      // 1. Get recommended model based on RAM
      let modelToUse = 'gemma3:1b'; // Fallback
      try {
        const recommended = await invoke<string>('builtin_ai_get_recommended_model');
        console.log('[SummaryModelDownloadStep] Recommended:', recommended);
        modelToUse = recommended;
      } catch (error) {
        console.error('[SummaryModelDownloadStep] RAM detection failed:', error);
        toast.info('Using default model (Gemma 3 1B)');
      }

      setRecommendedModel(modelToUse);
      updateDisplayInfo(modelToUse);

      // 2. Check for existing models
      const existingModel = await invoke<string | null>('builtin_ai_get_available_summary_model');

      if (existingModel) {
        console.log(`[SummaryModelDownloadStep] Using existing: ${existingModel}`);
        setSelectedSummaryModel(existingModel);
        setSummaryModelDownloaded(true);
        updateDisplayInfo(existingModel);
        setStatus('downloaded');
        return;
      }

      // 3. No existing model - check if recommended is ready
      const isReady = await invoke<boolean>('builtin_ai_is_model_ready', {
        modelName: modelToUse,
        refresh: true,
      });
      console.log(`[SummaryModelDownloadStep] ${modelToUse} ready:`, isReady);

      if (isReady) {
        setSummaryModelDownloaded(true);
        setSelectedSummaryModel(modelToUse);
        setStatus('downloaded');
        return;
      }

      // Model not ready, set for download
      setSelectedSummaryModel(modelToUse);
      setStatus('ready');
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : 'Initialization failed';
      console.error('[SummaryModelDownloadStep] Init error:', errorMsg);
      setSummaryModelError(errorMsg);
      setStatus('error');
    }
  };

  const downloadSummaryModel = async () => {
    if (!recommendedModel) return;

    try {
      setSummaryModelError(null);
      setStatus('downloading');
      const modelToDownload = selectedSummaryModel || recommendedModel;
      console.log(`[SummaryModelDownloadStep] Starting download: ${modelToDownload}`);

      await invoke('builtin_ai_download_model', {
        modelName: modelToDownload,
      });

      // Download complete (context listener will set summaryModelDownloaded)
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : 'Download failed';
      console.error(`[SummaryModelDownloadStep] Download error:`, errorMsg);
      setSummaryModelError(errorMsg);
      setStatus('error');
      toast.error('Failed to download Summary model', {
        description: errorMsg,
      });
    }
  };

  const retryDownload = async () => {
    setRetryCount(prev => prev + 1);
    setSummaryModelError(null);
    setStatus('ready');
  };

  const isDownloading = status === 'downloading';
  const isDownloaded = status === 'downloaded';
  const isError = status === 'error';
  const isChecking = status === 'checking';

  return (
    <OnboardingContainer
      title="Step 2"
      description={`Download Summary AI Model (${recommendedModel} - open source model by ${recommendedModel?.includes('gemma3') ? 'Google' : 'Mistral'})`}
      step={3}
      totalSteps={4}
      stepOffset={1}
    >
      <div className="flex flex-col items-center space-y-6">
        {/* Success State */}
        {isDownloaded && (
          <div className="w-full max-w-md bg-green-50 rounded-lg border border-green-200 p-6 text-center space-y-2">
            <CheckCircle2 className="w-12 h-12 text-green-600 mx-auto" />
            <h3 className="font-semibold text-gray-900">Model Ready!</h3>
            <p className="text-sm text-gray-600">{modelDisplayName} is ready to use</p>
          </div>
        )}

        {/* Checking State */}
        {isChecking && (
          <div className="w-full max-w-md bg-white rounded-lg border border-gray-200 p-6 space-y-4">
            <div className="flex items-center gap-3">
              <div className="flex size-12 items-center justify-center rounded-lg bg-gray-100">
                <Sparkles className="w-6 h-6 text-gray-700" />
              </div>
              <div className="flex-1">
                <h3 className="font-semibold text-gray-900">Summary Model</h3>
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
                <h3 className="font-semibold text-gray-900">Downloading Summary Model</h3>
                <p className="text-sm text-gray-600">({modelSize})</p>
              </div>
              <span className="text-sm font-medium text-gray-900">{Math.round(summaryModelProgress)}%</span>
            </div>
            <Progress value={summaryModelProgress} className="h-2" />
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
                <h3 className="font-semibold text-gray-900">{modelDisplayName}</h3>
                <p className="text-sm text-gray-600">{modelSize}</p>
              </div>
            </div>

            <div className="flex items-start gap-2 text-sm text-red-600 bg-red-50 p-3 rounded-lg">
              <AlertCircle className="w-4 h-4 mt-0.5 flex-shrink-0" />
              <div className="flex-1">
                <p>{summaryModelError}</p>
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
                Almost there!
              </>
            ) : isDownloading ? (
              <>
                <Loader2 className="w-4 h-4 mr-2 animate-spin" />
                Downloading...
              </>
            ) : (
              "Waiting for Download"
            )}
          </Button>
        </div>

      </div>
    </OnboardingContainer>
  );
}
