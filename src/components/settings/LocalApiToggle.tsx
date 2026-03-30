import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { ToggleSwitch } from "../ui/ToggleSwitch";

interface LocalApiToggleProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
  onToggle?: (enabled: boolean) => void;
}

export const LocalApiToggle: React.FC<LocalApiToggleProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false, onToggle }) => {
    const { t } = useTranslation();
    const [enabled, setEnabled] = useState(false);
    const [isUpdating, setIsUpdating] = useState(false);
    const [isLoaded, setIsLoaded] = useState(false);

    useEffect(() => {
      let cancelled = false;
      invoke<boolean>("get_local_api_enabled")
        .then((value) => {
          if (!cancelled) {
            setEnabled(value);
            setIsLoaded(true);
          }
        })
        .catch((error) => {
          console.error("Failed to get local API enabled state:", error);
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
          await invoke("set_local_api_enabled", { enabled: value });
          onToggle?.(value);
        } catch (error) {
          console.error("Failed to toggle local API:", error);
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
        label={t("settings.localApi.enable")}
        description={t("settings.localApi.enableDescription")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
