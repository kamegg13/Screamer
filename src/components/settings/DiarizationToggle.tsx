import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { ToggleSwitch } from "../ui/ToggleSwitch";

interface DiarizationToggleProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
  onToggle?: (enabled: boolean) => void;
}

export const DiarizationToggle: React.FC<DiarizationToggleProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false, onToggle }) => {
    const { t } = useTranslation();
    const [enabled, setEnabled] = useState(false);
    const [isUpdating, setIsUpdating] = useState(false);
    const [isLoaded, setIsLoaded] = useState(false);
    const [modelsAvailable, setModelsAvailable] = useState(true);

    useEffect(() => {
      let cancelled = false;

      const loadState = async () => {
        try {
          const [enabledValue, available] = await Promise.all([
            invoke<boolean>("get_diarization_enabled"),
            invoke<boolean>("get_diarization_models_available"),
          ]);
          if (!cancelled) {
            setEnabled(enabledValue);
            setModelsAvailable(available);
            setIsLoaded(true);
          }
        } catch (error) {
          console.error("Failed to get diarization state:", error);
          if (!cancelled) {
            setIsLoaded(true);
          }
        }
      };

      loadState();
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
          await invoke("set_diarization_enabled", { enabled: value });
          onToggle?.(value);
        } catch (error) {
          console.error("Failed to toggle diarization:", error);
          setEnabled(previousValue);
        } finally {
          setIsUpdating(false);
        }
      },
      [enabled, onToggle],
    );

    if (!isLoaded) return null;

    const description = modelsAvailable
      ? t("settings.diarization.enableDescription")
      : t("settings.diarization.modelsNotAvailable");

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={handleChange}
        isUpdating={isUpdating}
        disabled={!modelsAvailable}
        label={t("settings.diarization.enable")}
        description={description}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
