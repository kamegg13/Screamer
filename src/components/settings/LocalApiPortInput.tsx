import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { SettingContainer } from "../ui/SettingContainer";
import { Input } from "../ui/Input";

interface LocalApiPortInputProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const LocalApiPortInput: React.FC<LocalApiPortInputProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const [port, setPort] = useState<number>(5500);
    const [isLoaded, setIsLoaded] = useState(false);
    const [isSaving, setIsSaving] = useState(false);

    useEffect(() => {
      let cancelled = false;
      invoke<number>("get_local_api_port")
        .then((value) => {
          if (!cancelled) {
            setPort(value);
            setIsLoaded(true);
          }
        })
        .catch((error) => {
          console.error("Failed to get local API port:", error);
          if (!cancelled) {
            setIsLoaded(true);
          }
        });
      return () => {
        cancelled = true;
      };
    }, []);

    const handleBlur = useCallback(async () => {
      if (port < 1 || port > 65535) {
        return;
      }
      setIsSaving(true);
      try {
        await invoke("set_local_api_port", { port });
      } catch (error) {
        console.error("Failed to set local API port:", error);
      } finally {
        setIsSaving(false);
      }
    }, [port]);

    const handleKeyDown = useCallback(
      (e: React.KeyboardEvent<HTMLInputElement>) => {
        if (e.key === "Enter") {
          handleBlur();
        }
      },
      [handleBlur],
    );

    if (!isLoaded) return null;

    return (
      <SettingContainer
        title={t("settings.localApi.port")}
        description={t("settings.localApi.portDescription")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      >
        <Input
          type="number"
          min={1}
          max={65535}
          value={port}
          onChange={(e) => setPort(Number(e.target.value))}
          onBlur={handleBlur}
          onKeyDown={handleKeyDown}
          disabled={isSaving}
          variant="compact"
          className="w-24"
        />
      </SettingContainer>
    );
  },
);
