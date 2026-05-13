'use client';

import { useEffect, useRef, useState, useCallback } from 'react';
import { ChevronDown, Check } from 'lucide-react';

export interface AutoDetectBannerProps {
  mode: 'detect-prompt' | 'stop-prompt';
  initialTitle: string;
  candidateTitles: string[];
  onConfirm: (title: string) => void;
  onCancel: () => void;
  timeoutSeconds: number;
}

export function AutoDetectBanner({
  mode,
  initialTitle,
  candidateTitles,
  onConfirm,
  onCancel,
  timeoutSeconds,
}: AutoDetectBannerProps) {
  const [title, setTitle] = useState(initialTitle);
  const [remaining, setRemaining] = useState(timeoutSeconds);
  const [dropdownOpen, setDropdownOpen] = useState(false);
  const titleRef = useRef(title);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Keep ref in sync so the timeout callback always sees the latest title
  useEffect(() => {
    titleRef.current = title;
  }, [title]);

  useEffect(() => {
    setTitle(initialTitle);
    setRemaining(timeoutSeconds);
  }, [initialTitle, timeoutSeconds]);

  // Countdown: fires onConfirm(currentTitle) when it reaches 0
  useEffect(() => {
    timerRef.current = setInterval(() => {
      setRemaining(prev => {
        if (prev <= 1) {
          clearInterval(timerRef.current!);
          onConfirm(titleRef.current);
          return 0;
        }
        return prev - 1;
      });
    }, 1000);
    return () => {
      if (timerRef.current) clearInterval(timerRef.current);
    };
  }, [onConfirm]);

  const handleConfirm = useCallback(() => {
    if (timerRef.current) clearInterval(timerRef.current);
    onConfirm(title);
  }, [title, onConfirm]);

  const handleCancel = useCallback(() => {
    if (timerRef.current) clearInterval(timerRef.current);
    onCancel();
  }, [onCancel]);

  const handleSelectTitle = useCallback((t: string) => {
    setTitle(t);
    setDropdownOpen(false);
  }, []);

  const progress = timeoutSeconds > 0 ? remaining / timeoutSeconds : 0;
  const barColor = mode === 'detect-prompt' ? 'bg-blue-500' : 'bg-amber-500';

  if (mode === 'detect-prompt') {
    return (
      <div className="fixed top-4 left-1/2 -translate-x-1/2 z-50 w-[480px] bg-white rounded-xl shadow-2xl border border-gray-200 overflow-hidden">
        {/* Countdown progress bar */}
        <div
          className={`h-1 ${barColor} transition-all duration-1000 ease-linear`}
          style={{ width: `${progress * 100}%` }}
        />
        <div className="p-4">
          <div className="flex items-center justify-between mb-3">
            <span className="text-sm font-semibold text-gray-800">
              Google Meet detected — start recording?
            </span>
            <span className="text-xs text-gray-400 ml-2 shrink-0">
              auto-starts in {remaining}s
            </span>
          </div>

          {/* Editable title + candidate dropdown */}
          <div className="flex items-center gap-2 mb-3">
            <input
              type="text"
              value={title}
              onChange={e => setTitle(e.target.value)}
              className="flex-1 min-w-0 px-3 py-1.5 text-sm border border-gray-300 rounded-lg focus:outline-none focus:ring-2 focus:ring-blue-400"
              placeholder="Meeting title"
            />
            {candidateTitles.length > 1 && (
              <div className="relative shrink-0">
                <button
                  onClick={() => setDropdownOpen(o => !o)}
                  className="flex items-center gap-1 px-2 py-1.5 text-xs border border-gray-300 rounded-lg hover:bg-gray-50 transition-colors"
                  title="Choose from detected meeting titles"
                >
                  <ChevronDown className="w-3 h-3" />
                </button>
                {dropdownOpen && (
                  <div className="absolute right-0 mt-1 w-64 bg-white border border-gray-200 rounded-lg shadow-lg z-10 max-h-48 overflow-y-auto">
                    {candidateTitles.map(t => (
                      <button
                        key={t}
                        onClick={() => handleSelectTitle(t)}
                        className="w-full text-left px-3 py-2 text-xs hover:bg-gray-50 flex items-center justify-between gap-2"
                      >
                        <span className="truncate">{t}</span>
                        {t === title && <Check className="w-3 h-3 text-blue-500 shrink-0" />}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )}
          </div>

          <div className="flex gap-2">
            <button
              onClick={handleConfirm}
              className="flex-1 px-4 py-2 text-sm font-medium bg-blue-600 text-white rounded-lg hover:bg-blue-700 transition-colors"
            >
              Start Recording
            </button>
            <button
              onClick={handleCancel}
              className="px-4 py-2 text-sm font-medium text-gray-600 border border-gray-300 rounded-lg hover:bg-gray-50 transition-colors"
            >
              Cancel
            </button>
          </div>
        </div>
      </div>
    );
  }

  // stop-prompt
  return (
    <div className="fixed top-4 left-1/2 -translate-x-1/2 z-50 w-[420px] bg-white rounded-xl shadow-2xl border border-gray-200 overflow-hidden">
      <div
        className={`h-1 ${barColor} transition-all duration-1000 ease-linear`}
        style={{ width: `${progress * 100}%` }}
      />
      <div className="p-4">
        <div className="flex items-center justify-between mb-4">
          <span className="text-sm font-semibold text-gray-800">
            Google Meet call ended — stop recording?
          </span>
          <span className="text-xs text-gray-400 ml-2 shrink-0">
            stops in {remaining}s
          </span>
        </div>
        <div className="flex gap-2">
          <button
            onClick={handleConfirm}
            className="flex-1 px-4 py-2 text-sm font-medium bg-gray-800 text-white rounded-lg hover:bg-gray-900 transition-colors"
          >
            Stop Recording
          </button>
          <button
            onClick={handleCancel}
            className="px-4 py-2 text-sm font-medium text-gray-600 border border-gray-300 rounded-lg hover:bg-gray-50 transition-colors"
          >
            Keep Recording
          </button>
        </div>
      </div>
    </div>
  );
}
