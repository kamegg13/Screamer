import React, { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";

interface MaxSpeakersSelectorProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const MaxSpeakersSelector: React.FC<MaxSpeakersSelectorProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const [maxSpeakers, setMaxSpeakers] = useState<number>(5);
    const [isLoaded, setIsLoaded] = useState(false);

    useEffect(() => {
      let cancelled = false;
      invoke<number>("get_diarization_max_speakers")
        .then((value) => {
          if (!cancelled) {
            setMaxSpeakers(value);
            setIsLoaded(true);
          }
        })
        .catch((error) => {
          console.error("Failed to get max speakers:", error);
          if (!cancelled) {
            setIsLoaded(true);
          }
        });
      return () => {
        cancelled = true;
      };
    }, []);

    const options = useMemo(
      () =>
        Array.from({ length: 9 }, (_, i) => ({
          value: String(i + 2),
          label: String(i + 2),
        })),
      [],
    );

    const handleChange = useCallback(async (value: string) => {
      const numValue = Number(value);
      setMaxSpeakers(numValue);
      try {
        await invoke("set_diarization_max_speakers", {
          maxSpeakers: numValue,
        });
      } catch (error) {
        console.error("Failed to set max speakers:", error);
      }
    }, []);

    if (!isLoaded) return null;

    return (
      <SettingContainer
        title={t("settings.diarization.maxSpeakers")}
        description={t("settings.diarization.maxSpeakersDescription")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      >
        <Dropdown
          options={options}
          selectedValue={String(maxSpeakers)}
          onSelect={handleChange}
        />
      </SettingContainer>
    );
  });
