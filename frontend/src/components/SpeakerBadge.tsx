import React from "react";
import { Undo2 } from "lucide-react";
import { getSpeakerColor } from "@/services/speakerService";

interface SpeakerBadgeProps {
  name: string | null | undefined;
  colorIndex?: number;
  color?: string;
  isSuggestion?: boolean;
  canRevert?: boolean;
  onRevert?: () => void;
  onClick?: () => void;
}

function textColorForBackground(hsl: string): string {
  const match = hsl.match(/hsl\(\s*[\d.]+\s*,\s*[\d.]+%\s*,\s*([\d.]+)%\s*\)/);
  const lightness = match ? parseFloat(match[1]) : 55;
  return lightness < 50 ? "#ffffff" : "#000000";
}

function toHsla(hsl: string, alpha: number): string {
  return hsl.replace("hsl(", "hsla(").replace(")", `, ${alpha})`);
}

export function SpeakerBadge({
  name,
  colorIndex = 0,
  color,
  isSuggestion = false,
  canRevert = false,
  onRevert,
  onClick,
}: SpeakerBadgeProps) {
  const displayName = name?.trim() || "Unknown Speaker";
  const bgColor = color || getSpeakerColor(colorIndex);
  const textColor = textColorForBackground(bgColor);

  const baseClasses =
    "inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs font-medium max-w-[200px] truncate";
  const cursorClass = onClick ? "cursor-pointer hover:opacity-80" : "";
  const suggestionClass = isSuggestion ? "italic opacity-70" : "";

  return (
    <span
      className={`${baseClasses} ${cursorClass} ${suggestionClass} group`}
      style={{
        backgroundColor: toHsla(bgColor, 0.19),
        color: textColor,
        border: `1px solid ${toHsla(bgColor, 0.38)}`,
      }}
      onClick={onClick}
      role={onClick ? "button" : undefined}
      tabIndex={onClick ? 0 : undefined}
      onKeyDown={
        onClick
          ? (e) => {
              if (e.key === "Enter" || e.key === " ") onClick();
            }
          : undefined
      }
    >
      <span className="truncate">{displayName}</span>
      {canRevert && onRevert && (
        <span className="w-0 overflow-hidden group-hover:w-3.5 transition-[width] duration-200 ease-[cubic-bezier(0.34,1.56,0.64,1)] flex-shrink-0">
          <button
            type="button"
            className="opacity-60 hover:!opacity-100 transition-opacity duration-150"
            style={{ color: textColor }}
            onClick={(e) => {
              e.stopPropagation();
              onRevert();
            }}
            aria-label={`Revert ${displayName} to original label`}
            tabIndex={0}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.stopPropagation();
                onRevert();
              }
            }}
          >
            <Undo2 size={12} />
          </button>
        </span>
      )}
    </span>
  );
}

interface SpeakerLabelInputProps {
  onSubmit: (name: string) => void;
  onCancel: () => void;
  suggestions?: string[];
}

export function SpeakerLabelInput({
  onSubmit,
  onCancel,
  suggestions = [],
}: SpeakerLabelInputProps) {
  const [value, setValue] = React.useState("");

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" && value.trim()) {
      onSubmit(value.trim());
    } else if (e.key === "Escape") {
      onCancel();
    }
  };

  return (
    <div className="inline-flex flex-col gap-1">
      <input
        type="text"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={handleKeyDown}
        onBlur={onCancel}
        placeholder="Enter speaker name..."
        className="px-2 py-0.5 text-xs border rounded w-40"
        autoFocus
        maxLength={200}
      />
      {value.trim() === "" && (
        <span className="text-xs text-gray-400">Name required</span>
      )}
      {suggestions.length > 0 && (
        <div className="flex flex-wrap gap-1 mt-1">
          {suggestions
            .filter((s) =>
              s.toLowerCase().includes(value.toLowerCase())
            )
            .slice(0, 5)
            .map((s) => (
              <button
                key={s}
                type="button"
                className="text-xs px-1.5 py-0.5 rounded bg-gray-100 hover:bg-gray-200"
                onMouseDown={(e) => e.preventDefault()}
                // Keep focus on the input so onBlur cancel doesn't unmount the chip before its click fires.
                onClick={() => onSubmit(s)}
              >
                {s}
              </button>
            ))}
        </div>
      )}
    </div>
  );
}
