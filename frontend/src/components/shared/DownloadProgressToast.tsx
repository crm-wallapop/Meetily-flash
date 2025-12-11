'use client';

import React, { useEffect, useState, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import { X, Download, Check, Loader2, ArrowBigDownDash } from 'lucide-react';

interface DownloadProgress {
  modelName: string;
  displayName: string;
  progress: number;
  downloadedMb: number;
  totalMb: number;
  speedMbps: number;
  status: 'downloading' | 'completed' | 'error';
  error?: string;
}

// Custom toast component for download progress
function DownloadToastContent({
  download,
}: {
  download: DownloadProgress;
}) {
  const isComplete = download.status === 'completed';
  const hasError = download.status === 'error';

  return (
    <div className="flex items-start gap-3 w-full max-w-sm bg-white rounded-lg shadow-lg border border-gray-200 p-4">
      {/* Icon */}
      <div className={`flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center ${
        isComplete ? 'bg-green-100' : hasError ? 'bg-red-100' : 'bg-gray-100'
      }`}>
        {isComplete ? (
          <Check className="w-4 h-4 text-green-600" />
        ) : hasError ? (
          <X className="w-4 h-4 text-red-600" />
        ) : (
          <ArrowBigDownDash className="w-4 h-4 text-gray-900 animate-bounce" />
        )}
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center justify-between gap-2 mb-1">
          <p className="text-sm font-medium text-gray-900 truncate">
            {download.displayName}
          </p>
        </div>

        {hasError ? (
          <p className="text-xs text-red-600">{download.error || 'Download failed'}</p>
        ) : isComplete ? (
          <p className="text-xs text-green-600">Download complete</p>
        ) : (
          <>
            {/* Progress bar */}
            <div className="w-full h-1.5 bg-gray-200 rounded-full overflow-hidden mb-1.5">
              <div
                className="h-full bg-gray-900 rounded-full transition-all duration-300"
                style={{ width: `${download.progress}%` }}
              />
            </div>

            {/* Progress text */}
            <div className="flex items-center justify-between text-xs text-gray-500">
              <span>
                {download.downloadedMb.toFixed(1)} / {download.totalMb.toFixed(1)} MB
              </span>
              <span className="flex items-center gap-1">
                {download.speedMbps > 0 && (
                  <span>{download.speedMbps.toFixed(1)} MB/s</span>
                )}
                <span className="text-gray-900 font-medium">
                  {Math.round(download.progress)}%
                </span>
              </span>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

// Hook to manage download progress toasts
export function useDownloadProgressToast() {
  const [downloads, setDownloads] = useState<Map<string, DownloadProgress>>(new Map());
  const [dismissedModels, setDismissedModels] = useState<Set<string>>(new Set());

  const updateDownload = useCallback((modelName: string, data: Partial<DownloadProgress>) => {
    setDownloads((prev) => {
      const updated = new Map(prev);
      const existing = updated.get(modelName) || {
        modelName,
        displayName: modelName,
        progress: 0,
        downloadedMb: 0,
        totalMb: 0,
        speedMbps: 0,
        status: 'downloading' as const,
      };

      updated.set(modelName, { ...existing, ...data });
      return updated;
    });
  }, []);

  const showDownloadToast = useCallback((download: DownloadProgress) => {
    const toastId = `download-${download.modelName}`;

    if (download.status === 'completed') {
      toast.custom(
        (t) => (
          <DownloadToastContent
            download={download}
          />
        ),
        {
          position: 'top-right',
          id: toastId,
          duration: 3000, // Auto-dismiss completed toasts after 3s
        }
      );
    } else {
      toast.custom(
        (t) => (
          <DownloadToastContent
            download={download}
          />
        ),
        {
          position: 'top-right',
          id: toastId,
          duration: Infinity, // Keep showing until dismissed or completed
        }
      );
    }
  }, []);

  // Effect to handle toast visibility based on dismissed state
  useEffect(() => {
    downloads.forEach((download) => {
      // If model was dismissed and is still downloading, don't show it
      if (dismissedModels.has(download.modelName) && download.status === 'downloading') {
        return;
      }

      // If status changed to completed or error, we might want to show it even if dismissed previously
      // (Optional: remove from dismissed set if you want to force show completion)
      if (download.status === 'completed' || download.status === 'error') {
        if (dismissedModels.has(download.modelName)) {
           // Remove from dismissed so we can show the completion/error toast
           setDismissedModels(prev => {
             const next = new Set(prev);
             next.delete(download.modelName);
             return next;
           });
        }
      }

      showDownloadToast(download);
    });
  }, [downloads, dismissedModels, showDownloadToast]);

  // Listen to Parakeet download events
  useEffect(() => {
    const unlistenProgress = listen<{
      modelName: string;
      progress: number;
      downloaded_mb?: number;
      total_mb?: number;
      speed_mbps?: number;
      status?: string;
    }>('parakeet-model-download-progress', (event) => {
      const { modelName, progress, downloaded_mb, total_mb, speed_mbps, status } = event.payload;

      const downloadData: DownloadProgress = {
        modelName,
        displayName: 'Transcription Model (Parakeet)',
        progress,
        downloadedMb: downloaded_mb ?? 0,
        totalMb: total_mb ?? 670,
        speedMbps: speed_mbps ?? 0,
        status: status === 'completed' || progress >= 100 ? 'completed' : 'downloading',
      };

      updateDownload(modelName, downloadData);
      // Removed direct showDownloadToast call here, handled by effect
    });

    const unlistenComplete = listen<{ modelName: string }>(
      'parakeet-model-download-complete',
      (event) => {
        const { modelName } = event.payload;
        const downloadData: DownloadProgress = {
          modelName,
          displayName: 'Transcription Model (Parakeet)',
          progress: 100,
          downloadedMb: 670,
          totalMb: 670,
          speedMbps: 0,
          status: 'completed',
        };
        updateDownload(modelName, downloadData);
        // Removed direct showDownloadToast call here, handled by effect
      }
    );

    const unlistenError = listen<{ modelName: string; error: string }>(
      'parakeet-model-download-error',
      (event) => {
        const { modelName, error } = event.payload;
        const downloadData: DownloadProgress = {
          modelName,
          displayName: 'Transcription Model (Parakeet)',
          progress: 0,
          downloadedMb: 0,
          totalMb: 670,
          speedMbps: 0,
          status: 'error',
          error,
        };
        updateDownload(modelName, downloadData);
        // Removed direct showDownloadToast call here, handled by effect
      }
    );

    return () => {
      unlistenProgress.then((fn) => fn());
      unlistenComplete.then((fn) => fn());
      unlistenError.then((fn) => fn());
    };
  }, [updateDownload]); // Removed showDownloadToast dependency

  // Listen to Built-in AI (Gemma) download events
  useEffect(() => {
    const unlisten = listen<{
      model: string;
      progress: number;
      downloaded_mb?: number;
      total_mb?: number;
      speed_mbps?: number;
      status: string;
    }>('builtin-ai-download-progress', (event) => {
      const { model, progress, downloaded_mb, total_mb, speed_mbps, status } = event.payload;

      const downloadData: DownloadProgress = {
        modelName: model,
        displayName: `Summary Model (${model})`,
        progress,
        downloadedMb: downloaded_mb ?? 0,
        totalMb: total_mb ?? (model.includes('4b') ? 2500 : 806),
        speedMbps: speed_mbps ?? 0,
        status: status === 'completed' || progress >= 100 ? 'completed' : 'downloading',
      };

      updateDownload(model, downloadData);
      // Removed direct showDownloadToast call here, handled by effect
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [updateDownload]); // Removed showDownloadToast dependency

  return { downloads };
}

// Component to initialize download toast listeners at app level
export function DownloadProgressToastProvider() {
  useDownloadProgressToast();
  return null;
}
