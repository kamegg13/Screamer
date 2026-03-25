import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { Slider } from "../ui/Slider";

interface SystemAudioGainSliderProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
  disabled?: boolean;
}

export const SystemAudioGainSlider: React.FC<SystemAudioGainSliderProps> =
  React.memo(
    ({ descriptionMode = "tooltip", grouped = false, disabled = false }) => {
      const { t } = useTranslation();
      const [gain, setGain] = useState(1.0);
      const [isLoaded, setIsLoaded] = useState(false);

      useEffect(() => {
        let cancelled = false;
        invoke<number>("get_system_audio_gain")
          .then((value) => {
            if (!cancelled) {
              setGain(value);
              setIsLoaded(true);
            }
          })
          .catch((error) => {
            console.error("Failed to get system audio gain:", error);
            if (!cancelled) {
              setIsLoaded(true);
            }
          });
        return () => {
          cancelled = true;
        };
      }, []);

      const handleChange = async (value: number) => {
        setGain(value);
        try {
          await invoke("set_system_audio_gain", { gain: value });
        } catch (error) {
          console.error("Failed to set system audio gain:", error);
        }
      };

      if (!isLoaded) return null;

      return (
        <Slider
          value={gain}
          onChange={handleChange}
          min={0}
          max={2}
          step={0.1}
          label={t("settings.sound.systemAudio.gain")}
          description={t("settings.sound.systemAudio.gainDescription")}
          descriptionMode={descriptionMode}
          grouped={grouped}
          formatValue={(value) => `${Math.round(value * 100)}%`}
          disabled={disabled}
        />
      );
    },
  );
