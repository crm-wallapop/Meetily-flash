import React, { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Download, Loader2, CheckCircle2, AlertCircle } from 'lucide-react';
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

export function SummaryModelDownloadStep() {
  const {
    goNext,
    summaryModelDownloaded,
    summaryModelProgress,
    selectedSummaryModel,
    setSummaryModelDownloaded,
    setSelectedSummaryModel,
  } = useOnboarding();

  const [summaryModelError, setSummaryModelError] = useState<string | null>(null);
  const [isChecking, setIsChecking] = useState(true);
  const [recommendedModel, setRecommendedModel] = useState<string | null>(null);
  const [modelDisplayName, setModelDisplayName] = useState<string>('');
  const [modelSize, setModelSize] = useState<string>('');

  // Initialization effect
  useEffect(() => {
    initializeStep();
  }, []);

  // Auto-start download effect
  useEffect(() => {
    if (!isChecking && !summaryModelDownloaded && !summaryModelError && recommendedModel) {
      downloadSummaryModel();
    }
  }, [isChecking, summaryModelDownloaded, summaryModelError, recommendedModel]);


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
      setIsChecking(true);
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
        // Will auto-advance via useEffect
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
        // Will auto-advance via useEffect
        return;
      }

      // Model not ready, set for download
      setSelectedSummaryModel(modelToUse);
      // Will trigger download via useEffect

    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : 'Initialization failed';
      console.error('[SummaryModelDownloadStep] Init error:', errorMsg);
      setSummaryModelError(errorMsg);
    } finally {
      setIsChecking(false);
    }
  };

  const downloadSummaryModel = async () => {
    if (!recommendedModel) return;

    try {
      setSummaryModelError(null);
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
      toast.error('Failed to download Summary model', {
        description: errorMsg,
      });
    }
  };

  return (
    <OnboardingContainer
      title="Step 2"
      description="Download Summary AI model (gemma3 / mistral7b - open source by Google / Mistral)"
      step={4}
    >
      <div className="flex flex-col items-center space-y-6">
        {/* Summary Model Download Card */}
        <div className="w-full max-w-md bg-white rounded-lg border border-gray-200 p-6 space-y-4">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              {/* <Download className="w-5 h-5 text-gray-600" /> */}
              <div>
                <h3 className="font-medium text-gray-900">{modelDisplayName}</h3>
                <p className="text-sm text-gray-600">{modelSize}</p>
              </div>
            </div>
            {isChecking ? (
              <Loader2 className="w-6 h-6 text-gray-400 animate-spin" />
            ) : summaryModelDownloaded ? (
              <CheckCircle2 className="w-6 h-6 text-green-600" />
            ) : summaryModelError ? (
              <AlertCircle className="w-6 h-6 text-red-600" />
            ) : (
              <Loader2 className="w-6 h-6 text-gray-600 animate-spin" />
            )}
          </div>

          {!summaryModelDownloaded && summaryModelProgress > 0 && (
            <div className="space-y-2">
              <Progress value={summaryModelProgress} className="h-2" />
              <p className="text-xs text-center text-gray-500">
                {Math.round(summaryModelProgress)}%
              </p>
            </div>
          )}

          {summaryModelError && (
            <div className="bg-red-50 border border-red-200 rounded-lg p-3">
              <p className="text-sm text-red-800">{summaryModelError}</p>
              <Button
                onClick={downloadSummaryModel}
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
            disabled={!summaryModelDownloaded}
            className="w-full h-11 bg-gray-900 hover:bg-gray-800 text-white disabled:opacity-50 disabled:cursor-not-allowed"
          >
            Almost there!
          </Button>
        </div>
      </div>
    </OnboardingContainer>
  );
}
