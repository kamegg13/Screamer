import React, { useMemo } from "react";

const SPEAKER_COLORS = [
  { bg: "bg-blue-500/15", text: "text-blue-400", border: "border-blue-500/30" },
  {
    bg: "bg-green-500/15",
    text: "text-green-400",
    border: "border-green-500/30",
  },
  {
    bg: "bg-orange-500/15",
    text: "text-orange-400",
    border: "border-orange-500/30",
  },
  {
    bg: "bg-purple-500/15",
    text: "text-purple-400",
    border: "border-purple-500/30",
  },
  { bg: "bg-pink-500/15", text: "text-pink-400", border: "border-pink-500/30" },
  { bg: "bg-cyan-500/15", text: "text-cyan-400", border: "border-cyan-500/30" },
  {
    bg: "bg-yellow-500/15",
    text: "text-yellow-400",
    border: "border-yellow-500/30",
  },
  { bg: "bg-red-500/15", text: "text-red-400", border: "border-red-500/30" },
  {
    bg: "bg-indigo-500/15",
    text: "text-indigo-400",
    border: "border-indigo-500/30",
  },
  {
    bg: "bg-teal-500/15",
    text: "text-teal-400",
    border: "border-teal-500/30",
  },
] as const;

interface SpeakerSegment {
  readonly speaker: string;
  readonly speakerIndex: number;
  readonly text: string;
}

function parseDiarizedText(raw: string): readonly SpeakerSegment[] {
  const lines = raw.split("\n").filter((line) => line.trim().length > 0);
  const speakerMap = new Map<string, number>();
  let nextIndex = 0;

  return lines.map((line) => {
    const match = line.match(/^\[(.+?)\]\s*(.*)/);
    if (match) {
      const speaker = match[1];
      const text = match[2];

      if (!speakerMap.has(speaker)) {
        speakerMap.set(speaker, nextIndex);
        nextIndex += 1;
      }

      return {
        speaker,
        speakerIndex: speakerMap.get(speaker)!,
        text,
      };
    }

    return {
      speaker: "",
      speakerIndex: 0,
      text: line,
    };
  });
}

function getSpeakerColor(index: number) {
  return SPEAKER_COLORS[index % SPEAKER_COLORS.length];
}

interface DiarizedTextProps {
  readonly text: string;
}

export const DiarizedText: React.FC<DiarizedTextProps> = ({ text }) => {
  const segments = useMemo(() => parseDiarizedText(text), [text]);

  return (
    <div className="space-y-1.5 select-text cursor-text">
      {segments.map((segment, index) => {
        const color = getSpeakerColor(segment.speakerIndex);
        return (
          <div key={index} className="flex items-start gap-2">
            {segment.speaker && (
              <span
                className={`inline-flex items-center shrink-0 px-1.5 py-0.5 rounded text-[10px] font-semibold ${color.bg} ${color.text} border ${color.border}`}
              >
                {segment.speaker}
              </span>
            )}
            <span className="text-sm italic text-text/90">{segment.text}</span>
          </div>
        );
      })}
    </div>
  );
};
