import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { ToggleSwitch } from "../ui/ToggleSwitch";

interface SystemAudioToggleProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
  onToggle?: (enabled: boolean) => void;
}

export const SystemAudioToggle: React.FC<SystemAudioToggleProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false, onToggle }) => {
    const { t } = useTranslation();
    const [enabled, setEnabled] = useState(false);
    const [isUpdating, setIsUpdating] = useState(false);
    const [isLoaded, setIsLoaded] = useState(false);

    useEffect(() => {
      let cancelled = false;
      invoke<boolean>("get_system_audio_enabled")
        .then((value) => {
          if (!cancelled) {
            setEnabled(value);
            setIsLoaded(true);
          }
        })
        .catch((error) => {
          console.error("Failed to get system audio enabled state:", error);
          if (!cancelled) {
            setIsLoaded(true);
          }
        });
      return () => {
        cancelled = true;
      };
    }, []);

    const handleChange = useCallback(
      async (value: boolean) => {
        setIsUpdating(true);
        const previousValue = enabled;
        setEnabled(value);
        try {
          await invoke("toggle_system_audio", { enabled: value });
          onToggle?.(value);
        } catch (error) {
          console.error("Failed to toggle system audio:", error);
          setEnabled(previousValue);
        } finally {
          setIsUpdating(false);
        }
      },
      [enabled, onToggle],
    );

    if (!isLoaded) return null;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={handleChange}
        isUpdating={isUpdating}
        label={t("settings.sound.systemAudio.capture")}
        description={t("settings.sound.systemAudio.captureDescription")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
